//! Reads a PBF into whitelisted features. Three passes: relations, then ways and nodes in
//! the order the node-location strategy needs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use omp_core::whitelist::{ElementType, GeometryKind, Whitelist};
use osmpbf::{BlobDecode, BlobReader, Element, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use serde::Serialize;

use super::geom::{self, Assembly, BBox, Geometry, P};
use super::nodes::{DenseIndex, NodeSet, SparseLocations};

pub struct Feature {
    /// Source element: OSM id × 4 + 0 node, 1 way, 2 relation, 3 other source. Features are
    /// sorted by `(layer, id)` so results do not depend on parallel read order.
    pub id: i64,
    pub layer: u8,
    pub class: u16,
    pub geom: Geometry,
    /// `(index into the layer's props, value)`.
    pub props: Vec<(u8, Box<str>)>,
}

/// A road way kept for the routing graph.
pub struct Road {
    pub way_id: i64,
    pub class: u16,
    pub refs: Vec<i64>,
    pub coords: Vec<P>,
    /// 1 forward only, -1 backward only, 0 both ways.
    pub oneway: i8,
    pub maxspeed_kmh: Option<f64>,
    /// False for `access=no|private`.
    pub routable: bool,
}

#[derive(Default, Clone, Serialize)]
pub struct ClassCount {
    pub nodes: u64,
    pub ways: u64,
    pub relations: u64,
    /// Point features made from areas, by area: <500, <2000, <10000, >=10000 m².
    pub area_m2_buckets: [u64; 4],
}

#[derive(Default, Serialize)]
pub struct AssemblyStats {
    pub relations: u64,
    pub ok: u64,
    /// At least one member way is not in the extract (usually cut at the region border).
    pub missing_members: u64,
    /// All members present but they do not join into closed rings.
    pub broken: u64,
}

#[derive(Default, Serialize)]
pub struct ReadStats {
    pub nodes: u64,
    pub ways: u64,
    pub relations: u64,
    pub missing_node_refs: u64,
    pub unclosed_area_ways: u64,
    pub restriction_relations: u64,
    pub multipolygons: AssemblyStats,
    pub admin_boundaries: AssemblyStats,
    /// Road ways carrying each routing tag.
    pub road_tags: BTreeMap<String, u64>,
    pub needed_nodes: u64,
    pub node_set_heap_bytes: u64,
    pub node_index_disk_bytes: u64,
}

pub struct OsmData {
    pub bbox: Option<BBox>,
    pub features: Vec<Feature>,
    pub roads: Vec<Road>,
    /// `[layer][class]`.
    pub class_counts: Vec<Vec<ClassCount>>,
    pub tag_values: BTreeMap<String, BTreeMap<String, u64>>,
    pub stats: ReadStats,
}

pub enum Strategy<'a> {
    Dense(&'a Path),
    Sparse,
}

/// Keys whose every value is counted, matched or not, so whitelist changes can be judged.
const REPORT_KEYS: &[&str] = &[
    "aeroway", "amenity", "boundary", "highway", "landuse", "leisure", "natural", "place",
    "railway", "route", "service", "tourism", "waterway",
];
const ROAD_TAGS: &[&str] = &["oneway", "maxspeed", "toll", "access"];

struct Ctx<'a> {
    wl: &'a Whitelist,
    keys: HashSet<&'a str>,
    roads_layer: usize,
    boundaries_layer: usize,
}

impl Ctx<'_> {
    fn interesting(&self, tags: &[(&str, &str)]) -> bool {
        tags.iter()
            .any(|(k, _)| self.keys.contains(k) || REPORT_KEYS.contains(k))
    }
}

struct Out {
    layer: u8,
    class: u16,
    kind: GeometryKind,
    props: Vec<(u8, Box<str>)>,
}

struct WayRec {
    id: i64,
    refs: Vec<i64>,
    outs: Vec<Out>,
    member: bool,
    road: Option<(u16, i8, Option<f64>, bool)>,
}

struct RelRec {
    id: i64,
    outs: Vec<Out>,
    members: Vec<(i64, bool)>,
}

struct AdminRel {
    members: Vec<(i64, bool)>,
}

#[derive(Default)]
struct Block {
    nodes: u64,
    ways: u64,
    relations: u64,
    restrictions: u64,
    unclosed: u64,
    missing_refs: u64,
    features: Vec<Feature>,
    way_recs: Vec<WayRec>,
    needed: Vec<(i64, P)>,
    member_coords: Vec<(i64, Vec<P>)>,
    roads: Vec<Road>,
    rels: Vec<RelRec>,
    admin: Vec<AdminRel>,
    /// `(way id, boundary class)`.
    boundary_ways: Vec<(i64, u16)>,
    counts: Vec<Vec<ClassCount>>,
    tag_values: HashMap<String, HashMap<String, u64>>,
    road_tags: HashMap<&'static str, u64>,
}

impl Block {
    fn new(wl: &Whitelist) -> Self {
        Block {
            counts: class_counts(wl),
            ..Default::default()
        }
    }

    fn count_tags(&mut self, tags: &[(&str, &str)]) {
        for (k, v) in tags {
            if REPORT_KEYS.contains(k) {
                let m = self.tag_values.entry((*k).to_owned()).or_default();
                match m.get_mut(*v) {
                    Some(n) => *n += 1,
                    None => {
                        m.insert((*v).to_owned(), 1);
                    }
                }
            }
        }
    }
}

fn class_counts(wl: &Whitelist) -> Vec<Vec<ClassCount>> {
    wl.layers
        .iter()
        .map(|l| vec![ClassCount::default(); l.classes.len()])
        .collect()
}

/// Runs `f` over every data block of the file in parallel.
fn pass<F>(path: &Path, wl: &Whitelist, f: F) -> Result<Vec<Block>>
where
    F: Fn(&PrimitiveBlock, &mut Block) -> Result<()> + Sync,
{
    BlobReader::from_path(path)
        .with_context(|| format!("open {}", path.display()))?
        .par_bridge()
        .map(|blob| {
            let mut out = Block::new(wl);
            if let BlobDecode::OsmData(block) = blob?.decode()? {
                f(&block, &mut out)?;
            }
            Ok(out)
        })
        .collect()
}

fn header_bbox(path: &Path) -> Result<Option<BBox>> {
    let mut reader = BlobReader::from_path(path)?;
    if let Some(blob) = reader.next()
        && let BlobDecode::OsmHeader(h) = blob?.decode()?
    {
        return Ok(h.bbox().map(|b| BBox {
            min: geom::project(b.top, b.left),
            max: geom::project(b.bottom, b.right),
        }));
    }
    Ok(None)
}

fn props(layer: &omp_core::whitelist::Layer, tags: &[(&str, &str)]) -> Vec<(u8, Box<str>)> {
    layer
        .props
        .iter()
        .enumerate()
        .filter_map(|(i, k)| {
            tags.iter()
                .find(|(tk, _)| tk == k)
                .map(|(_, v)| (i as u8, Box::from(*v)))
        })
        .collect()
}

/// The layers and classes an element of type `ty` with `tags` belongs to.
fn classify(
    ctx: &Ctx,
    ty: ElementType,
    tags: &[(&str, &str)],
    counts: &mut [Vec<ClassCount>],
) -> Vec<Out> {
    let mut outs = Vec::new();
    for (li, layer) in ctx.wl.layers.iter().enumerate() {
        if li == ctx.boundaries_layer || !layer.from.contains(&ty) {
            continue;
        }
        if let Some(ci) = layer.classify(tags) {
            let c = &mut counts[li][ci];
            match ty {
                ElementType::Node => c.nodes += 1,
                ElementType::Way => c.ways += 1,
                ElementType::Relation => c.relations += 1,
            }
            outs.push(Out {
                layer: li as u8,
                class: ci as u16,
                kind: layer.geometry_of(ci),
                props: props(layer, tags),
            });
        }
    }
    outs
}

fn parse_maxspeed(v: &str) -> Option<f64> {
    let v = v.trim();
    if let Some(mph) = v.strip_suffix("mph") {
        return mph.trim().parse::<f64>().ok().map(|s| s * 1.609_344);
    }
    v.parse().ok()
}

pub fn read(path: &Path, wl: &Whitelist, strategy: Strategy) -> Result<OsmData> {
    let ctx = Ctx {
        wl,
        keys: wl.keys().into_iter().collect(),
        roads_layer: wl
            .layer_index("roads")
            .context("whitelist has no roads layer")?,
        boundaries_layer: wl
            .layer_index("boundaries")
            .context("whitelist has no boundaries layer")?,
    };
    let bbox = header_bbox(path)?;
    let mut stats = ReadStats::default();
    let mut counts = class_counts(wl);
    let mut tag_values: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut merge_common = |b: &mut Block, stats: &mut ReadStats| {
        stats.nodes += b.nodes;
        stats.ways += b.ways;
        stats.relations += b.relations;
        stats.restriction_relations += b.restrictions;
        stats.unclosed_area_ways += b.unclosed;
        stats.missing_node_refs += b.missing_refs;
        for (l, cs) in b.counts.iter().enumerate() {
            for (c, n) in cs.iter().enumerate() {
                let t = &mut counts[l][c];
                t.nodes += n.nodes;
                t.ways += n.ways;
                t.relations += n.relations;
            }
        }
        for (k, vs) in b.tag_values.drain() {
            let m = tag_values.entry(k).or_default();
            for (v, n) in vs {
                *m.entry(v).or_default() += n;
            }
        }
        for (k, n) in b.road_tags.drain() {
            *stats.road_tags.entry(k.to_owned()).or_default() += n;
        }
    };

    // Pass 1: relations.
    let mut rels = Vec::new();
    let mut admin = Vec::new();
    let mut boundary_ways: HashMap<i64, u16> = HashMap::new();
    for mut b in pass(path, wl, |block, out| relations_block(&ctx, block, out))? {
        merge_common(&mut b, &mut stats);
        rels.append(&mut b.rels);
        admin.append(&mut b.admin);
        for (id, class) in b.boundary_ways {
            boundary_ways
                .entry(id)
                .and_modify(|c| *c = (*c).min(class))
                .or_insert(class);
        }
    }
    let members: HashSet<i64> = rels
        .iter()
        .flat_map(|r| &r.members)
        .chain(admin.iter().flat_map(|a| &a.members))
        .map(|(id, _)| *id)
        .collect();
    let way_pass = |b: &PrimitiveBlock, out: &mut Block| {
        ways_block(&ctx, b, out, &members, &boundary_ways);
        Ok(())
    };

    let mut features = Vec::new();
    let mut roads = Vec::new();
    let mut member_coords: HashMap<i64, Vec<P>> = HashMap::new();
    let mut collect = |b: &mut Block| {
        features.append(&mut b.features);
        roads.append(&mut b.roads);
        member_coords.extend(b.member_coords.drain(..));
    };
    match strategy {
        Strategy::Dense(index_path) => {
            // Pass 2: every node into the index. Pass 3: ways, resolved as they are read.
            let index = DenseIndex::create(index_path)?;
            for mut b in pass(path, wl, |block, out| {
                nodes_block(&ctx, block, out, |id, p| index.set(id, p).map(|_| false))
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
            stats.node_index_disk_bytes = index.disk_bytes();
            for mut b in pass(path, wl, |block, out| {
                way_pass(block, out)?;
                for rec in std::mem::take(&mut out.way_recs) {
                    resolve_way(rec, |id| index.get(id), out);
                }
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
        }
        Strategy::Sparse => {
            // Pass 2: ways, marking the nodes they need. Pass 3: only those nodes.
            let mut way_recs = Vec::new();
            for mut b in pass(path, wl, way_pass)? {
                merge_common(&mut b, &mut stats);
                way_recs.append(&mut b.way_recs);
            }
            let mut needed = NodeSet::new();
            for r in &way_recs {
                for &id in &r.refs {
                    needed.insert(id);
                }
            }
            stats.node_set_heap_bytes = needed.heap_bytes();
            let mut found = Vec::new();
            for mut b in pass(path, wl, |block, out| {
                nodes_block(&ctx, block, out, |id, _| Ok(needed.contains(id)))
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
                found.append(&mut b.needed);
            }
            drop(needed);
            let locs = SparseLocations::new(found);
            stats.needed_nodes = locs.len() as u64;
            let resolved: Vec<Block> = way_recs
                .into_par_iter()
                .fold(
                    || Block::new(wl),
                    |mut out, rec| {
                        resolve_way(rec, |id| locs.get(id), &mut out);
                        out
                    },
                )
                .collect();
            for mut b in resolved {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
        }
    }

    // Multipolygons and admin boundaries from their member ways.
    let lookup = |members: &[(i64, bool)]| -> Option<Vec<(&[P], bool)>> {
        members
            .iter()
            .map(|(id, inner)| member_coords.get(id).map(|c| (c.as_slice(), *inner)))
            .collect()
    };
    let assemble = |members: &[(i64, bool)], s: &mut AssemblyStats| -> Option<Vec<Vec<P>>> {
        s.relations += 1;
        let Some(ways) = lookup(members) else {
            s.missing_members += 1;
            return None;
        };
        match geom::assemble(&ways) {
            Assembly::Ok(rings) => {
                s.ok += 1;
                Some(rings)
            }
            Assembly::Broken(rings) => {
                s.broken += 1;
                Some(rings).filter(|r| !r.is_empty())
            }
        }
    };
    for r in rels {
        let Some(rings) = assemble(&r.members, &mut stats.multipolygons) else {
            continue;
        };
        for o in r.outs {
            let g = match o.kind {
                GeometryKind::Area => Geometry::Polygon(rings.clone()),
                GeometryKind::Point => {
                    let area = geom::area_m2(&rings);
                    let c = &mut counts[o.layer as usize][o.class as usize];
                    c.area_m2_buckets[area_bucket(area)] += 1;
                    Geometry::Point(geom::centroid(&rings))
                }
                GeometryKind::Line => continue,
            };
            features.push(Feature {
                id: r.id * 4 + 2,
                layer: o.layer,
                class: o.class,
                geom: g,
                props: o.props,
            });
        }
    }
    features.sort_unstable_by_key(|f| (f.layer, f.id, f.class));
    roads.sort_unstable_by_key(|r| r.way_id);
    for a in &admin {
        assemble(&a.members, &mut stats.admin_boundaries);
    }
    // Area buckets for POIs made from closed ways were counted per block.
    Ok(OsmData {
        bbox,
        features,
        roads,
        class_counts: counts,
        tag_values,
        stats,
    })
}

fn area_bucket(m2: f64) -> usize {
    match m2 {
        a if a < 500.0 => 0,
        a if a < 2000.0 => 1,
        a if a < 10_000.0 => 2,
        _ => 3,
    }
}

fn relations_block(ctx: &Ctx, block: &PrimitiveBlock, out: &mut Block) -> Result<()> {
    let mut tags = Vec::new();
    for el in block.elements() {
        let Element::Relation(r) = el else { continue };
        out.relations += 1;
        tags.clear();
        tags.extend(r.tags());
        let ty = tags.iter().find(|(k, _)| *k == "type").map(|(_, v)| *v);
        if ty == Some("restriction") {
            out.restrictions += 1;
            continue;
        }
        if !matches!(ty, Some("multipolygon" | "boundary")) || !ctx.interesting(&tags) {
            continue;
        }
        out.count_tags(&tags);
        let ways = || -> Result<Vec<(i64, bool)>> {
            let mut v = Vec::new();
            for m in r.members() {
                if m.member_type == RelMemberType::Way {
                    match m.role()? {
                        "outer" | "" => v.push((m.member_id, false)),
                        "inner" => v.push((m.member_id, true)),
                        _ => {}
                    }
                }
            }
            Ok(v)
        };
        let bl = &ctx.wl.layers[ctx.boundaries_layer];
        if let Some(ci) = bl.classify(&tags) {
            out.counts[ctx.boundaries_layer][ci].relations += 1;
            let members = ways()?;
            out.boundary_ways
                .extend(members.iter().map(|(id, _)| (*id, ci as u16)));
            out.admin.push(AdminRel { members });
        }
        // Protected areas are often `type=boundary`, so both types are tried as areas.
        let outs = classify(ctx, ElementType::Relation, &tags, &mut out.counts);
        if !outs.is_empty() {
            out.rels.push(RelRec {
                id: r.id(),
                outs,
                members: ways()?,
            });
        }
    }
    Ok(())
}

fn ways_block(
    ctx: &Ctx,
    block: &PrimitiveBlock,
    out: &mut Block,
    members: &HashSet<i64>,
    boundary_ways: &HashMap<i64, u16>,
) {
    let mut tags = Vec::new();
    for el in block.elements() {
        let Element::Way(w) = el else { continue };
        out.ways += 1;
        tags.clear();
        tags.extend(w.tags());
        let interesting = ctx.interesting(&tags);
        if interesting {
            out.count_tags(&tags);
        }
        let mut outs = if interesting {
            classify(ctx, ElementType::Way, &tags, &mut out.counts)
        } else {
            Vec::new()
        };
        let mut road = None;
        if let Some(o) = outs.iter().find(|o| o.layer as usize == ctx.roads_layer) {
            let get = |k: &str| tags.iter().find(|(tk, _)| *tk == k).map(|(_, v)| *v);
            for k in ROAD_TAGS {
                if get(k).is_some() {
                    *out.road_tags.entry(k).or_default() += 1;
                }
            }
            let class = &ctx.wl.layers[ctx.roads_layer].classes[o.class as usize].name;
            let oneway = match get("oneway") {
                Some("yes" | "1" | "true") => 1,
                Some("-1" | "reverse") => -1,
                Some("no") => 0,
                _ if get("junction") == Some("roundabout") || class == "motorway" => 1,
                _ => 0,
            };
            let routable = !matches!(get("access"), Some("no" | "private"));
            road = Some((
                o.class,
                oneway,
                get("maxspeed").and_then(parse_maxspeed),
                routable,
            ));
        }
        if let Some(&class) = boundary_ways.get(&w.id()) {
            let bl = ctx.boundaries_layer;
            let level = ctx.wl.layers[bl].classes[class as usize]
                .name
                .trim_start_matches("admin_");
            let mut props = vec![(0u8, Box::from(level))];
            if let Some((_, v)) = tags.iter().find(|(k, _)| *k == "maritime") {
                props.push((1, Box::from(*v)));
            }
            outs.push(Out {
                layer: bl as u8,
                class,
                kind: GeometryKind::Line,
                props,
            });
        }
        let member = members.contains(&w.id());
        if outs.is_empty() && !member {
            continue;
        }
        out.way_recs.push(WayRec {
            id: w.id(),
            refs: w.refs().collect(),
            outs,
            member,
            road,
        });
    }
}

/// `node` stores or checks a node location and says whether to keep it as "needed".
fn nodes_block(
    ctx: &Ctx,
    block: &PrimitiveBlock,
    out: &mut Block,
    node: impl Fn(i64, P) -> Result<bool>,
) -> Result<()> {
    let mut tags = Vec::new();
    let visit = |out: &mut Block, id: i64, lat: i32, lon: i32, tags: &[(&str, &str)]| {
        out.nodes += 1;
        let p = geom::project_e7(lat, lon);
        if node(id, p)? {
            out.needed.push((id, p));
        }
        if !tags.is_empty() && ctx.interesting(tags) {
            out.count_tags(tags);
            for o in classify(ctx, ElementType::Node, tags, &mut out.counts) {
                out.features.push(Feature {
                    id: id * 4,
                    layer: o.layer,
                    class: o.class,
                    geom: Geometry::Point(p),
                    props: o.props,
                });
            }
        }
        anyhow::Ok(())
    };
    for el in block.elements() {
        tags.clear();
        match el {
            Element::DenseNode(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags)?;
            }
            Element::Node(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_way(rec: WayRec, get: impl Fn(i64) -> Option<P>, out: &mut Block) {
    let mut refs = Vec::with_capacity(rec.refs.len());
    let mut coords = Vec::with_capacity(rec.refs.len());
    for id in rec.refs {
        match get(id) {
            Some(p) => {
                refs.push(id);
                coords.push(p);
            }
            None => out.missing_refs += 1,
        }
    }
    let closed = coords.len() >= 4 && coords.first() == coords.last();
    for o in rec.outs {
        let g = match o.kind {
            GeometryKind::Line if coords.len() >= 2 => Geometry::Lines(vec![coords.clone()]),
            GeometryKind::Area => match geom::closed_way_ring(&coords, true) {
                Some(ring) => Geometry::Polygon(vec![ring]),
                None => {
                    out.unclosed += 1;
                    continue;
                }
            },
            GeometryKind::Point if closed => {
                let rings = [geom::closed_way_ring(&coords, true).unwrap_or_default()];
                let area = geom::area_m2(&rings);
                out.counts[o.layer as usize][o.class as usize].area_m2_buckets
                    [area_bucket(area)] += 1;
                Geometry::Point(geom::centroid(&rings))
            }
            GeometryKind::Point if !coords.is_empty() => Geometry::Point(coords[coords.len() / 2]),
            _ => continue,
        };
        out.features.push(Feature {
            id: rec.id * 4 + 1,
            layer: o.layer,
            class: o.class,
            geom: g,
            props: o.props,
        });
    }
    if let Some((class, oneway, maxspeed_kmh, routable)) = rec.road {
        out.roads.push(Road {
            way_id: rec.id,
            class,
            refs,
            coords: coords.clone(),
            oneway,
            maxspeed_kmh,
            routable,
        });
    }
    if rec.member {
        out.member_coords.push((rec.id, coords));
    }
}
