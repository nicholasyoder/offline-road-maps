//! The graph's three PBF passes: turn restrictions, then the ways the car profile keeps,
//! then the locations and tags of their nodes.

use std::collections::HashMap;

use anyhow::Result;
use omp_core::profile::{self, Dropped};
use osmpbf::{Element, RelMemberType};
use serde::{Deserialize, Serialize};

use super::{Input, RestrictionIn, Via, WayIn};
use crate::geom::{self, P};
use crate::pbf::nodes::{NodeSet, RankedLocations};
use crate::pbf::{Kinds, Pbf};

#[derive(Default, Serialize, Deserialize)]
pub struct ReadStats {
    /// Ways with a routable `highway` value, and ferries.
    pub ways_read: u64,
    pub ferries: u64,
    pub dropped_access: u64,
    pub dropped_closed: u64,
    pub dropped_size: u64,
    pub dropped_speed: u64,
    /// Ways left with fewer than two nodes found.
    pub dropped_short: u64,
    pub missing_node_refs: u64,
    pub signals: u64,
    /// Barriers a car passes (gates, toll booths, cattle grids, …).
    pub passable_barriers: u64,
    pub toll_booths: u64,
    pub blocking_barriers: u64,
    /// `type=restriction` relations, for any vehicle.
    pub restriction_relations: u64,
    /// Of those, restrictions for cars without the members they need: no `from` or `to`
    /// way, or no `via`, or both a via node and via ways.
    pub restrictions_unusable: u64,
}

#[derive(Default)]
struct WayBlock {
    ways: Vec<(i64, profile::Way, Vec<i64>)>,
    stats: ReadStats,
}

#[derive(Default)]
struct RelBlock {
    list: Vec<RestrictionIn>,
    relations: u64,
    unusable: u64,
}

pub fn read(pbf: &Pbf) -> Result<Input> {
    let mut stats = ReadStats::default();

    let mut restrictions = Vec::new();
    for b in pbf.pass(
        "restrictions",
        Kinds::RELATIONS,
        RelBlock::default,
        |block, out| {
            let mut tags = Vec::new();
            for el in block.elements() {
                let Element::Relation(r) = el else { continue };
                tags.clear();
                tags.extend(r.tags());
                if !tags.contains(&("type", "restriction")) {
                    continue;
                }
                out.relations += 1;
                let Some(kind) = profile::restriction(&tags) else {
                    continue;
                };
                let (mut from, mut to, mut via_ways, mut via_node) = (vec![], vec![], vec![], None);
                let mut extra_via = false;
                for m in r.members() {
                    match (m.role()?, m.member_type) {
                        ("from", RelMemberType::Way) => from.push(m.member_id),
                        ("to", RelMemberType::Way) => to.push(m.member_id),
                        ("via", RelMemberType::Way) => via_ways.push(m.member_id),
                        ("via", RelMemberType::Node) => {
                            extra_via |= via_node.is_some();
                            via_node = Some(m.member_id);
                        }
                        _ => {}
                    }
                }
                let via = match (via_node, via_ways.is_empty()) {
                    (Some(n), true) if !extra_via => Via::Node(n),
                    (None, false) => Via::Ways(via_ways),
                    _ => {
                        out.unusable += 1;
                        continue;
                    }
                };
                if from.is_empty() || to.is_empty() {
                    out.unusable += 1;
                    continue;
                }
                out.list.push(RestrictionIn {
                    id: r.id(),
                    kind,
                    from,
                    via,
                    to,
                });
            }
            Ok(())
        },
    )? {
        restrictions.extend(b.list);
        stats.restriction_relations += b.relations;
        stats.restrictions_unusable += b.unusable;
    }
    restrictions.sort_unstable_by_key(|r| r.id);

    let mut blocks = pbf.pass(
        "graph ways",
        Kinds::WAYS,
        WayBlock::default,
        |block, out| {
            let mut tags = Vec::new();
            for el in block.elements() {
                let Element::Way(w) = el else { continue };
                tags.clear();
                tags.extend(w.tags());
                if !tags.iter().any(|(k, _)| matches!(*k, "highway" | "route")) {
                    continue;
                }
                let s = &mut out.stats;
                match profile::way(&tags) {
                    Ok(way) => {
                        s.ways_read += 1;
                        s.ferries += u64::from(way.ferry);
                        out.ways.push((w.id(), way, w.refs().collect()));
                    }
                    Err(Dropped::NotRoad) => {}
                    Err(d) => {
                        s.ways_read += 1;
                        *match d {
                            Dropped::Access => &mut s.dropped_access,
                            Dropped::Closed => &mut s.dropped_closed,
                            Dropped::Size => &mut s.dropped_size,
                            _ => &mut s.dropped_speed,
                        } += 1;
                    }
                }
            }
            Ok(())
        },
    )?;
    blocks.sort_unstable_by_key(|b| b.ways.first().map(|w| w.0));
    let total: usize = blocks.iter().flat_map(|b| &b.ways).map(|w| w.2.len()).sum();
    let needed = NodeSet::new();
    let mut raw = Vec::with_capacity(blocks.iter().map(|b| b.ways.len()).sum());
    let mut refs = Vec::with_capacity(total);
    for b in blocks {
        stats.ways_read += b.stats.ways_read;
        stats.ferries += b.stats.ferries;
        stats.dropped_access += b.stats.dropped_access;
        stats.dropped_closed += b.stats.dropped_closed;
        stats.dropped_size += b.stats.dropped_size;
        stats.dropped_speed += b.stats.dropped_speed;
        for (id, way, r) in b.ways {
            for &n in &r {
                needed.insert(n);
            }
            raw.push((id, way, refs.len()..refs.len() + r.len()));
            refs.extend(r);
        }
    }

    let locs = RankedLocations::new(needed);
    let mut nodes = HashMap::new();
    for b in pbf.pass(
        "graph nodes",
        Kinds::NODES,
        Vec::new,
        |block, out: &mut Vec<(i64, profile::Node)>| {
            let mut tags = Vec::new();
            let mut visit = |id: i64, lat: i32, lon: i32, tags: &[(&str, &str)]| {
                if !locs.contains(id) {
                    return;
                }
                locs.set(id, geom::project_e7(lat, lon));
                if let Some(n) = profile::node(tags) {
                    out.push((id, n));
                }
            };
            for el in block.elements() {
                tags.clear();
                match el {
                    Element::DenseNode(n) => {
                        tags.extend(n.tags());
                        visit(n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
                    }
                    Element::Node(n) => {
                        tags.extend(n.tags());
                        visit(n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
                    }
                    _ => {}
                }
            }
            Ok(())
        },
    )? {
        for (id, n) in b {
            stats.signals += u64::from(n.signal);
            stats.toll_booths += u64::from(n.toll);
            if n.block {
                stats.blocking_barriers += 1;
            } else if n.delay_s > 0 && !n.signal || n.toll {
                stats.passable_barriers += 1;
            }
            nodes.insert(id, n);
        }
    }

    // Locations, leaving out the nodes not in the file; ways left with one node go. In place,
    // so in the order of `refs`.
    let mut coords: Vec<P> = Vec::with_capacity(refs.len());
    let mut kept = 0;
    let mut ways = Vec::with_capacity(raw.len());
    for (id, way, range) in raw {
        let start = kept;
        for i in range {
            match locs.get(refs[i]) {
                Some(p) => {
                    refs[kept] = refs[i];
                    coords.push(p);
                    kept += 1;
                }
                None => stats.missing_node_refs += 1,
            }
        }
        if kept - start < 2 {
            coords.truncate(start);
            kept = start;
            stats.dropped_short += 1;
            continue;
        }
        ways.push(WayIn::new(id, &way, start..kept));
    }
    refs.truncate(kept);
    refs.shrink_to_fit();
    ways.sort_unstable_by_key(|w| w.id);
    Ok(Input {
        ways,
        refs,
        coords,
        nodes,
        restrictions,
        read: stats,
    })
}
