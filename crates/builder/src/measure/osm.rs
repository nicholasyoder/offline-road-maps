//! Reads a PBF into whitelisted features. Relations first, then ways and nodes in the order
//! the node-location strategy needs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use omp_core::whitelist::{ElementType, GeometryKind, Whitelist};
use osmpbf::{BlobDecode, BlobReader, Element, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use serde::Serialize;

use super::geom::{self, Assembly, BBox, Geometry, P};
use super::nodes::{DenseIndex, NodeSet, RankedLocations};
use super::poly::Poly;
use super::routing::RoutingStats;
use super::tiles::{self, Band};

pub struct Feature {
    /// Source element: OSM id × 4 + 0 node, 1 way, 2 relation, 3 other source. Features are
    /// sorted by `(layer, id)` so results do not depend on parallel read order.
    pub id: i64,
    pub layer: u8,
    pub class: u16,
    pub geom: Geometry,
    /// `(index into the layer's props, value)`.
    pub props: Vec<(u8, Box<str>)>,
    /// Raises the class's minzoom for this feature (label points of small areas).
    pub minzoom: Option<u8>,
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

impl AssemblyStats {
    fn add(&mut self, o: &AssemblyStats) {
        self.relations += o.relations;
        self.ok += o.ok;
        self.missing_members += o.missing_members;
        self.broken += o.broken;
    }
}

fn add_counts(to: &mut [Vec<ClassCount>], from: &[Vec<ClassCount>]) {
    for (tl, fl) in to.iter_mut().zip(from) {
        for (t, n) in tl.iter_mut().zip(fl) {
            t.nodes += n.nodes;
            t.ways += n.ways;
            t.relations += n.relations;
            for (a, b) in t.area_m2_buckets.iter_mut().zip(n.area_m2_buckets) {
                *a += b;
            }
        }
    }
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
    pub routes: RouteStats,
    pub needed_nodes: u64,
    pub node_set_heap_bytes: u64,
    /// Bitset, rank index and one 8-byte location per needed node.
    pub node_locations_heap_bytes: u64,
    pub node_index_disk_bytes: u64,
}

/// Route relation coverage, for shields (counted in band 0 only).
#[derive(Default, Serialize)]
pub struct RouteStats {
    /// `type=route` + `route=road` relations with a `network` and a `ref`.
    pub relations: u64,
    /// Region → road class (motorway, trunk, primary, secondary, tertiary) → ways in a route
    /// relation, with only a `ref`, and with neither; then of all those, the ways whose first
    /// route has a shield of its own (not [`Shield::Generic`](omp_core::routes::Shield)).
    pub coverage: BTreeMap<String, BTreeMap<String, [u64; 4]>>,
    /// Network → road ways on its routes (a way on two routes of one network counts twice).
    pub networks: BTreeMap<String, u64>,
}

/// Where a road way's `routes` come from, as an index into [`RouteStats::coverage`].
#[derive(Clone, Copy)]
enum RouteSource {
    Relation = 0,
    Ref = 1,
    None = 2,
}

/// The index in [`RouteStats::coverage`] of the ways with a real shield.
const REAL_SHIELD: usize = 3;

/// Smallest area a label point is made for, in square pixels at its first zoom.
const LABEL_MIN_PX2: f64 = 1024.0;
/// World units² per square pixel at z14 on a 512 px tile: (2^(32 − 14 − 9))².
const Z14_PX2: f64 = (1u64 << 18) as f64;
/// Props the builder fills instead of copying a tag of that name (see the whitelist).
const COMPUTED_PROPS: &[&str] = &["routes", "area"];

/// A label point for a named area: the point, its first zoom (where the area covers
/// [`LABEL_MIN_PX2`]), and log2 of the area in square pixels at z14. `None` if the area is
/// under [`LABEL_MIN_PX2`] even at z16.
pub fn label_point(rings: &[Vec<P>]) -> Option<(P, u8, u32)> {
    let px2 = rings.iter().map(|r| geom::ring_area(r)).sum::<f64>() / Z14_PX2;
    if px2 * 16.0 < LABEL_MIN_PX2 || !px2.is_finite() {
        return None;
    }
    // Each zoom out quarters the area.
    let steps = ((px2 / LABEL_MIN_PX2).log2() / 2.0).floor().max(0.0) as u32;
    let minzoom = 14u32.saturating_sub(steps) as u8;
    let b = BBox::of(rings.iter().flatten().copied());
    let span = f64::from((b.max[0] - b.min[0]).max(b.max[1] - b.min[1]));
    let precision = (span / 200.0).max(256.0);
    // Simplified to a quarter of the precision first: big lakes and rivers have hundreds of
    // thousands of vertices, and polylabel measures the distance to every edge per cell.
    let k = (64 - (span as u64).leading_zeros()).saturating_sub(30);
    let simple: Vec<Vec<P>> = rings
        .iter()
        .filter_map(|r| {
            let mut local: Vec<[i32; 2]> = r
                .iter()
                .map(|p| {
                    [
                        ((p[0] - b.min[0]) >> k) as i32,
                        ((p[1] - b.min[1]) >> k) as i32,
                    ]
                })
                .collect();
            local.push(local[0]);
            let mut s = geom::simplify(&local, precision / 4.0 / f64::from(1u32 << k));
            s.pop();
            (s.len() >= 3).then(|| {
                s.iter()
                    .map(|q| {
                        [
                            ((q[0] as u32) << k) + b.min[0],
                            ((q[1] as u32) << k) + b.min[1],
                        ]
                    })
                    .collect()
            })
        })
        .collect();
    let p = match simple.is_empty() {
        true => geom::polylabel(rings, precision, 4096),
        false => geom::polylabel(&simple, precision, 4096),
    };
    Some((p, minzoom, px2.log2().floor().max(0.0) as u32))
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
    /// The `labels` layer and its `area` prop.
    labels: Option<(u8, Option<u8>)>,
    /// The roads layer's `routes` prop.
    routes_prop: Option<u8>,
    /// `[road class]`: counted in [`RouteStats::coverage`].
    shield_class: Vec<bool>,
    /// Regions for [`RouteStats::coverage`]; empty counts everything as `all`.
    regions: &'a [(String, Poly)],
    band: Band,
    minzoom: Vec<Vec<u8>>,
}

impl Ctx<'_> {
    /// Whether this run's band needs the feature.
    fn keeps(&self, f: &Feature) -> bool {
        self.band.n == 1
            || self
                .band
                .keeps(f.geom.bbox(), tiles::feature_minzoom(&self.minzoom, f))
    }

    /// The feature for an `o` made from closed rings: an area, a label point, or a centroid
    /// (with its area counted).
    fn area_feature(
        &self,
        id: i64,
        mut o: Out,
        rings: Vec<Vec<P>>,
        counts: &mut [Vec<ClassCount>],
    ) -> Option<Feature> {
        let mut minzoom = None;
        let g = match o.kind {
            GeometryKind::Area => Geometry::Polygon(rings),
            GeometryKind::Point if self.labels.is_some_and(|(l, _)| l == o.layer) => {
                let (p, z, area) = label_point(&rings)?;
                minzoom = Some(z);
                if let Some((_, Some(k))) = self.labels {
                    o.props.push((k, area.to_string().into()));
                    o.props.sort_unstable_by_key(|(k, _)| *k);
                }
                Geometry::Point(p)
            }
            GeometryKind::Point => {
                let area = geom::area_m2(&rings);
                counts[o.layer as usize][o.class as usize].area_m2_buckets[area_bucket(area)] += 1;
                Geometry::Point(geom::centroid(&rings))
            }
            GeometryKind::Line => return None,
        };
        Some(Feature {
            id,
            layer: o.layer,
            class: o.class,
            geom: g,
            props: o.props,
            minzoom,
        })
    }

    /// The region of a way, for [`RouteStats::coverage`].
    fn region(&self, coords: &[P]) -> &str {
        if self.regions.is_empty() {
            return "all";
        }
        let Some(&mid) = coords.get(coords.len() / 2) else {
            return "other";
        };
        self.regions
            .iter()
            .find(|(_, poly)| poly.contains(mid))
            .map_or("other", |(name, _)| name)
    }

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
    /// For road ways: where `routes` came from, the class if it is counted in the coverage,
    /// the way's route relations (indices into [`Rels::routes`]), and whether its first route
    /// has a real shield.
    route: Option<(RouteSource, Option<u16>, Vec<u32>, bool)>,
}

/// What the relation pass found, for the way passes.
#[derive(Default)]
struct Rels {
    /// Member ways of multipolygons and admin boundaries.
    members: HashSet<i64>,
    /// `(way id, boundary class)`.
    boundary_ways: HashMap<i64, u16>,
    /// Way → its road routes, as indices into `routes`.
    way_routes: HashMap<i64, Vec<u32>>,
    /// Each distinct road route: `(network, ref)`.
    routes: Vec<(Box<str>, Box<str>)>,
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
    member_coords: Vec<(i64, Vec<P>)>,
    roads: Vec<Road>,
    rels: Vec<RelRec>,
    admin: Vec<AdminRel>,
    /// `(way id, boundary class)`.
    boundary_ways: Vec<(i64, u16)>,
    /// Road route relations: `(network, ref, member ways)`.
    route_rels: Vec<(Box<str>, Box<str>, Vec<i64>)>,
    /// `(region, road class, source or REAL_SHIELD)` → ways, for [`RouteStats::coverage`].
    route_coverage: HashMap<(String, u16, usize), u64>,
    /// Route index → ways, for [`RouteStats::networks`].
    route_ways: HashMap<u32, u64>,
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
    Ok(header_bounds(path)?.map(|[w, s, e, n]| BBox {
        min: geom::project(n, w),
        max: geom::project(s, e),
    }))
}

/// The PBF header's bounding box: west, south, east, north in degrees.
pub fn header_bounds(path: &Path) -> Result<Option<[f64; 4]>> {
    let mut reader = BlobReader::from_path(path)?;
    if let Some(blob) = reader.next()
        && let BlobDecode::OsmHeader(h) = blob?.decode()?
    {
        return Ok(h.bbox().map(|b| [b.left, b.bottom, b.right, b.top]));
    }
    Ok(None)
}

fn props(layer: &omp_core::whitelist::Layer, tags: &[(&str, &str)]) -> Vec<(u8, Box<str>)> {
    layer
        .props
        .iter()
        .enumerate()
        .filter(|(_, k)| !COMPUTED_PROPS.contains(&k.as_str()))
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

impl<'a> Ctx<'a> {
    fn new(wl: &'a Whitelist, band: Band, regions: &'a [(String, Poly)]) -> Result<Self> {
        let roads_layer = wl
            .layer_index("roads")
            .context("whitelist has no roads layer")?;
        let prop = |layer: usize, name: &str| {
            wl.layers[layer]
                .props
                .iter()
                .position(|p| p == name)
                .map(|i| i as u8)
        };
        let labels = wl.layer_index("labels").map(|l| (l as u8, prop(l, "area")));
        Ok(Ctx {
            wl,
            band,
            regions,
            minzoom: tiles::minzooms(wl),
            keys: wl.keys().into_iter().collect(),
            roads_layer,
            routes_prop: prop(roads_layer, "routes"),
            shield_class: wl.layers[roads_layer]
                .classes
                .iter()
                .map(|c| {
                    ["motorway", "trunk", "primary", "secondary", "tertiary"]
                        .contains(&c.name.as_str())
                })
                .collect(),
            labels,
            boundaries_layer: wl
                .layer_index("boundaries")
                .context("whitelist has no boundaries layer")?,
        })
    }
}

/// Counts the car routing graph like [`super::routing::build`] does, without keeping the roads:
/// two more passes over the ways, one marking nodes that are intersections or way ends and
/// one counting the edges between them. For a continent, where the roads do not fit next to
/// everything else.
pub fn count_road_graph(path: &Path, wl: &Whitelist, restrictions: u64) -> Result<RoutingStats> {
    let ctx = Ctx::new(wl, Band::ALL, &[])?;
    let rels = Rels::default();
    // `f` sees each road way's `(refs, oneway, routable)`.
    let roads = |f: &(dyn Fn(&[i64], i8, bool) + Sync)| {
        pass(path, wl, |block, out| {
            ways_block(&ctx, block, out, &rels);
            for rec in out.way_recs.drain(..) {
                if let Some((_, oneway, _, routable)) = rec.road {
                    f(&rec.refs, oneway, routable);
                }
            }
            Ok(())
        })
    };
    let routable = |refs: &[i64], ok: bool| ok && refs.len() >= 2;
    let (seen, graph_nodes) = (NodeSet::new(), NodeSet::new());
    let [road_ways, routable_ways, edges, oneway_segments] =
        [(); 4].map(|_| std::sync::atomic::AtomicU64::new(0));
    let add = |n: &std::sync::atomic::AtomicU64, v: u64| {
        n.fetch_add(v, std::sync::atomic::Ordering::Relaxed);
    };
    roads(&|refs, _, ok| {
        add(&road_ways, 1);
        if !routable(refs, ok) {
            return;
        }
        add(&routable_ways, 1);
        for &id in refs {
            if seen.insert(id) {
                graph_nodes.insert(id);
            }
        }
        graph_nodes.insert(refs[0]);
        graph_nodes.insert(refs[refs.len() - 1]);
    })?;
    drop(seen);
    roads(&|refs, oneway, ok| {
        if !routable(refs, ok) {
            return;
        }
        let mut from = refs[0];
        for &id in &refs[1..] {
            if !graph_nodes.contains(id) {
                continue;
            }
            if from != id {
                if oneway == 0 {
                    add(&edges, 2);
                } else {
                    add(&edges, 1);
                    add(&oneway_segments, 1);
                }
            }
            from = id;
        }
    })?;
    Ok(RoutingStats {
        road_ways: road_ways.into_inner(),
        routable_ways: routable_ways.into_inner(),
        nodes: graph_nodes.len(),
        edges: edges.into_inner(),
        oneway_segments: oneway_segments.into_inner(),
        restriction_relations: restrictions,
        ch: None,
    })
}

/// `keep_roads`: also return the road ways with their coordinates, for
/// [`super::routing::build`]. `band`: keep only the features that band needs. `regions`:
/// the regions [`RouteStats::coverage`] is split by.
pub fn read(
    path: &Path,
    wl: &Whitelist,
    strategy: Strategy,
    keep_roads: bool,
    band: Band,
    regions: &[(String, Poly)],
) -> Result<OsmData> {
    let ctx = Ctx::new(wl, band, regions)?;
    let progress = |what: &str| eprintln!("    {what}: peak RSS {:.0} MiB", super::peak_rss_mib());
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
        add_counts(&mut counts, &b.counts);
        for (k, vs) in b.tag_values.drain() {
            let m = tag_values.entry(k).or_default();
            for (v, n) in vs {
                *m.entry(v).or_default() += n;
            }
        }
        for (k, n) in b.road_tags.drain() {
            *stats.road_tags.entry(k.to_owned()).or_default() += n;
        }
        let roads = &wl.layers[ctx.roads_layer];
        for ((region, class, src), n) in b.route_coverage.drain() {
            let class = roads.classes[class as usize].name.clone();
            stats
                .routes
                .coverage
                .entry(region)
                .or_default()
                .entry(class)
                .or_default()[src] += n;
        }
    };
    let mut route_ways: HashMap<u32, u64> = HashMap::new();

    // Pass 1: relations.
    let mut rels = Vec::new();
    let mut admin = Vec::new();
    let mut index = Rels::default();
    let mut route_ids: HashMap<(Box<str>, Box<str>), u32> = HashMap::new();
    for mut b in pass(path, wl, |block, out| relations_block(&ctx, block, out))? {
        merge_common(&mut b, &mut stats);
        rels.append(&mut b.rels);
        admin.append(&mut b.admin);
        for (id, class) in b.boundary_ways {
            index
                .boundary_ways
                .entry(id)
                .and_modify(|c| *c = (*c).min(class))
                .or_insert(class);
        }
        // Blocks arrive in parallel order; route ids are only used within this run, and
        // `routes` values are sorted, so the order does not reach the pack.
        for (network, reference, ways) in b.route_rels {
            stats.routes.relations += 1;
            let next = index.routes.len() as u32;
            let id = *route_ids
                .entry((network.clone(), reference.clone()))
                .or_insert_with(|| {
                    index.routes.push((network, reference));
                    next
                });
            for w in ways {
                let v = index.way_routes.entry(w).or_default();
                if !v.contains(&id) {
                    v.push(id);
                }
            }
        }
    }
    drop(route_ids);
    index.members = rels
        .iter()
        .flat_map(|r| &r.members)
        .chain(admin.iter().flat_map(|a| &a.members))
        .map(|(id, _)| *id)
        .collect();
    let way_pass = |b: &PrimitiveBlock, out: &mut Block| {
        ways_block(&ctx, b, out, &index);
        anyhow::Ok(())
    };

    let mut features = Vec::new();
    let mut roads = Vec::new();
    let mut member_coords: HashMap<i64, Vec<P>> = HashMap::new();
    let mut collect = |b: &mut Block| {
        features.append(&mut b.features);
        roads.append(&mut b.roads);
        member_coords.extend(b.member_coords.drain(..));
        for (id, n) in b.route_ways.drain() {
            *route_ways.entry(id).or_default() += n;
        }
    };
    match strategy {
        Strategy::Dense(index_path) => {
            // Pass 2: every node into the index. Pass 3: ways, resolved as they are read.
            let dense = DenseIndex::create(index_path)?;
            for mut b in pass(path, wl, |block, out| {
                nodes_block(&ctx, block, out, |id, p| dense.set(id, p))?;
                out.features.retain(|f| ctx.keeps(f));
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
            stats.node_index_disk_bytes = dense.disk_bytes();
            for mut b in pass(path, wl, |block, out| {
                way_pass(block, out)?;
                for rec in std::mem::take(&mut out.way_recs) {
                    resolve_way(&ctx, rec, |id| dense.get(id), keep_roads, out);
                }
                out.features.retain(|f| ctx.keeps(f));
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
        }
        Strategy::Sparse => {
            // Pass 2: ways, marking the nodes they need. Pass 3: those nodes, stored by
            // rank. Pass 4: the ways again, resolved as they are read. Reading the ways twice
            // is cheaper than holding them, since their refs alone are 8 bytes per vertex.
            let needed = NodeSet::new();
            for mut b in pass(path, wl, |block, out| {
                way_pass(block, out)?;
                for rec in out.way_recs.drain(..) {
                    for &id in &rec.refs {
                        needed.insert(id);
                    }
                }
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
            }
            progress("ways marked");
            stats.node_set_heap_bytes = needed.heap_bytes();
            let locs = RankedLocations::new(needed);
            stats.needed_nodes = locs.len() as u64;
            stats.node_locations_heap_bytes = locs.heap_bytes();
            eprintln!(
                "    {} needed nodes, {:.1} GiB of locations",
                locs.len(),
                locs.heap_bytes() as f64 / f64::from(1 << 30)
            );
            for mut b in pass(path, wl, |block, out| {
                nodes_block(&ctx, block, out, |id, p| {
                    locs.set(id, p);
                    Ok(())
                })?;
                out.features.retain(|f| ctx.keeps(f));
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
            progress("nodes located");
            for mut b in pass(path, wl, |block, out| {
                // Tag and class counts were taken in pass 2.
                let mut ways = Block::new(wl);
                way_pass(block, &mut ways)?;
                for rec in ways.way_recs {
                    resolve_way(&ctx, rec, |id| locs.get(id), keep_roads, out);
                }
                out.features.retain(|f| ctx.keeps(f));
                Ok(())
            })? {
                merge_common(&mut b, &mut stats);
                collect(&mut b);
            }
            progress("ways resolved");
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
    // In parallel, for the label points of big areas.
    let (rel_features, assembly, rel_counts) = rels
        .into_par_iter()
        .fold(
            || (Vec::new(), AssemblyStats::default(), class_counts(wl)),
            |(mut fs, mut s, mut c), r| {
                if let Some(rings) = assemble(&r.members, &mut s) {
                    for o in r.outs {
                        if let Some(f) = ctx.area_feature(r.id * 4 + 2, o, rings.clone(), &mut c)
                            && ctx.keeps(&f)
                        {
                            fs.push(f);
                        }
                    }
                }
                (fs, s, c)
            },
        )
        .reduce(
            || (Vec::new(), AssemblyStats::default(), class_counts(wl)),
            |(mut fa, mut sa, mut ca), (mut fb, sb, cb)| {
                fa.append(&mut fb);
                sa.add(&sb);
                add_counts(&mut ca, &cb);
                (fa, sa, ca)
            },
        );
    features.extend(rel_features);
    stats.multipolygons.add(&assembly);
    add_counts(&mut counts, &rel_counts);
    for (id, n) in route_ways {
        let network = &index.routes[id as usize].0;
        *stats
            .routes
            .networks
            .entry(network.to_string())
            .or_default() += n;
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
        if ty == Some("route") {
            road_route(ctx, &r, &tags, out)?;
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

/// A `type=route` + `route=road` relation with a `network` and a `ref`: its member ways.
/// A `ref` of several routes (`1;2`) counts as one route each.
fn road_route(
    ctx: &Ctx,
    r: &osmpbf::Relation,
    tags: &[(&str, &str)],
    out: &mut Block,
) -> Result<()> {
    let get = |k: &str| tags.iter().find(|(tk, _)| *tk == k).map(|(_, v)| *v);
    if ctx.routes_prop.is_none() || get("route") != Some("road") {
        return Ok(());
    }
    let (Some(network), Some(reference)) = (get("network"), get("ref")) else {
        return Ok(());
    };
    let network = network.trim();
    if network.is_empty() || network.contains([';', '=']) {
        return Ok(());
    }
    let ways: Vec<i64> = r
        .members()
        .filter(|m| m.member_type == RelMemberType::Way)
        .map(|m| m.member_id)
        .collect();
    for reference in reference
        .split(';')
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        out.route_rels.push((
            network.into(),
            reference.replace('=', " ").into(),
            ways.clone(),
        ));
    }
    Ok(())
}

fn ways_block(ctx: &Ctx, block: &PrimitiveBlock, out: &mut Block, rels: &Rels) {
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
        let mut route = None;
        if let Some(o) = outs
            .iter_mut()
            .find(|o| o.layer as usize == ctx.roads_layer)
        {
            let get = |k: &str| tags.iter().find(|(tk, _)| *tk == k).map(|(_, v)| *v);
            if let Some(k) = ctx.routes_prop {
                let ids = rels.way_routes.get(&w.id()).cloned().unwrap_or_default();
                let (src, mut list) = if !ids.is_empty() {
                    let list = ids
                        .iter()
                        .map(|&i| {
                            let (n, r) = &rels.routes[i as usize];
                            (n.to_string(), r.to_string())
                        })
                        .collect();
                    (RouteSource::Relation, list)
                } else if let Some(r) = get("ref") {
                    (RouteSource::Ref, omp_core::routes::from_ref(r))
                } else {
                    (RouteSource::None, Vec::new())
                };
                if !list.is_empty() {
                    o.props
                        .push((k, omp_core::routes::format(&mut list).into()));
                    o.props.sort_unstable_by_key(|(k, _)| *k);
                }
                let real = list.first().is_some_and(|(n, _)| {
                    omp_core::routes::shield(n) != omp_core::routes::Shield::Generic
                });
                let counted = ctx.shield_class[o.class as usize].then_some(o.class);
                route = Some((src, counted, ids, real));
            }
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
        if let Some(&class) = rels.boundary_ways.get(&w.id()) {
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
        let member = rels.members.contains(&w.id());
        if outs.is_empty() && !member {
            continue;
        }
        out.way_recs.push(WayRec {
            id: w.id(),
            refs: w.refs().collect(),
            outs,
            member,
            road,
            route,
        });
    }
}

/// `node` stores a node location.
fn nodes_block(
    ctx: &Ctx,
    block: &PrimitiveBlock,
    out: &mut Block,
    node: impl Fn(i64, P) -> Result<()>,
) -> Result<()> {
    let mut tags = Vec::new();
    let visit = |out: &mut Block, id: i64, lat: i32, lon: i32, tags: &[(&str, &str)]| {
        out.nodes += 1;
        let p = geom::project_e7(lat, lon);
        node(id, p)?;
        if !tags.is_empty() && ctx.interesting(tags) {
            out.count_tags(tags);
            for o in classify(ctx, ElementType::Node, tags, &mut out.counts) {
                out.features.push(Feature {
                    id: id * 4,
                    layer: o.layer,
                    class: o.class,
                    geom: Geometry::Point(p),
                    props: o.props,
                    minzoom: None,
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

fn resolve_way(
    ctx: &Ctx,
    rec: WayRec,
    get: impl Fn(i64) -> Option<P>,
    keep_roads: bool,
    out: &mut Block,
) {
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
    if let Some((src, counted, ids, real)) = &rec.route
        && ctx.band.k == 0
    {
        if let Some(class) = *counted {
            let region = ctx.region(&coords);
            let key = (region.to_owned(), class, *src as usize);
            *out.route_coverage.entry(key).or_default() += 1;
            if *real {
                let key = (region.to_owned(), class, REAL_SHIELD);
                *out.route_coverage.entry(key).or_default() += 1;
            }
        }
        for &id in ids {
            *out.route_ways.entry(id).or_default() += 1;
        }
    }
    let ring = geom::closed_way_ring(&coords, true);
    let is_label = |o: &Out| ctx.labels.is_some_and(|(l, _)| l == o.layer);
    for o in rec.outs {
        let id = rec.id * 4 + 1;
        let g = match o.kind {
            GeometryKind::Line if coords.len() >= 2 => Geometry::Lines(vec![coords.clone()]),
            GeometryKind::Area | GeometryKind::Point if ring.is_some() => {
                let rings = vec![ring.clone().unwrap_or_default()];
                if let Some(f) = ctx.area_feature(id, o, rings, &mut out.counts) {
                    out.features.push(f);
                }
                continue;
            }
            GeometryKind::Area => {
                out.unclosed += 1;
                continue;
            }
            GeometryKind::Point if !coords.is_empty() && !is_label(&o) => {
                Geometry::Point(coords[coords.len() / 2])
            }
            _ => continue,
        };
        out.features.push(Feature {
            id,
            layer: o.layer,
            class: o.class,
            geom: g,
            props: o.props,
            minzoom: None,
        });
    }
    if keep_roads && let Some((class, oneway, maxspeed_kmh, routable)) = rec.road {
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
