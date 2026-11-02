//! Reads a PBF into whitelisted features: the relations, the ways (marking the nodes they
//! need), those nodes, then the ways again with their nodes' locations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use omp_core::whitelist::{ElementType, GeometryKind, Whitelist};
use osmpbf::{Element, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use serde::Serialize;

use super::nodes::{NodeSet, RankedLocations};
use super::{Kinds, Pbf};
use crate::geom::{self, Assembly, BBox, Geometry, P};
use crate::peak_rss_mib;
use crate::poly::Poly;
use crate::tiles::{self, Band};

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
    /// The [`KeepTags`] of the features of its layers, by [`Feature::id`]; only features
    /// that have some.
    pub kept_tags: HashMap<i64, Vec<(String, String)>>,
    pub routes: RouteStats,
}

/// Tags to keep for the features of some layers (search's alternative names), so they need
/// no pass of their own. Values are split at `;`.
pub struct KeepTags {
    pub layers: Vec<u8>,
    pub keys: Vec<&'static str>,
}

impl KeepTags {
    /// The tags of `keys`, if any of `outs` is in `layers`.
    fn of(&self, outs: &[Out], tags: &[(&str, &str)]) -> Vec<(String, String)> {
        if !outs.iter().any(|o| self.layers.contains(&o.layer)) {
            return Vec::new();
        }
        tags.iter()
            .filter(|(k, _)| self.keys.contains(k))
            .flat_map(|(k, v)| {
                v.split(';')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(move |v| ((*k).to_owned(), v.to_owned()))
            })
            .collect()
    }
}

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
    keep_tags: Option<&'a KeepTags>,
}

impl Ctx<'_> {
    /// Whether this run's band needs the feature.
    fn keeps(&self, f: &Feature) -> bool {
        self.band.n == 1
            || self
                .band
                .keeps(f.geom.bbox(), tiles::feature_minzoom(&self.minzoom, f))
    }

    /// The feature for an `o` made from closed rings: an area, a label point, or a centroid.
    fn area_feature(&self, id: i64, mut o: Out, rings: Vec<Vec<P>>) -> Option<Feature> {
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
            GeometryKind::Point => Geometry::Point(geom::centroid(&rings)),
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

    /// Whether a class could match `tags`: every class matches on keys of [`Whitelist::keys`].
    fn interesting(&self, tags: &[(&str, &str)]) -> bool {
        tags.iter().any(|(k, _)| self.keys.contains(k))
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
    /// [`KeepTags`].
    kept: Vec<(String, String)>,
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
    /// [`KeepTags`].
    kept: Vec<(String, String)>,
}

#[derive(Default)]
struct Block {
    features: Vec<Feature>,
    /// `(Feature::id, tags)`: [`KeepTags`].
    kept_tags: Vec<(i64, Vec<(String, String)>)>,
    way_recs: Vec<WayRec>,
    member_coords: Vec<(i64, Vec<P>)>,
    rels: Vec<RelRec>,
    /// `(way id, boundary class)`.
    boundary_ways: Vec<(i64, u16)>,
    /// Road route relations: `(network, ref, member ways)`.
    route_rels: Vec<(Box<str>, Box<str>, Vec<i64>)>,
    /// `(region, road class, source or REAL_SHIELD)` → ways, for [`RouteStats::coverage`].
    route_coverage: HashMap<(String, u16, usize), u64>,
    /// Route index → ways, for [`RouteStats::networks`].
    route_ways: HashMap<u32, u64>,
}

/// [`super::pass`], each block into a fresh [`Block`].
fn blocks<F>(pbf: &Pbf, what: &str, kinds: Kinds, f: F) -> Result<Vec<Block>>
where
    F: Fn(&PrimitiveBlock, &mut Block) -> Result<()> + Sync,
{
    pbf.pass(what, kinds, Block::default, f)
}

fn header_bbox(path: &Path) -> Result<Option<BBox>> {
    Ok(super::header_bounds(path)?.map(|[w, s, e, n]| BBox {
        min: geom::project(n, w),
        max: geom::project(s, e),
    }))
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
fn classify(ctx: &Ctx, ty: ElementType, tags: &[(&str, &str)]) -> Vec<Out> {
    let mut outs = Vec::new();
    for (li, layer) in ctx.wl.layers.iter().enumerate() {
        if li == ctx.boundaries_layer || !layer.from.contains(&ty) {
            continue;
        }
        if let Some(ci) = layer.classify(tags) {
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

impl<'a> Ctx<'a> {
    fn new(
        wl: &'a Whitelist,
        band: Band,
        regions: &'a [(String, Poly)],
        keep_tags: Option<&'a KeepTags>,
    ) -> Result<Self> {
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
            keep_tags,
        })
    }
}

/// `marked`: the nodes the ways need, if an earlier band marked them; else they are marked
/// and kept here for the next band. The ways do not depend on the band, so later bands skip
/// the marking pass. `band`: keep only the features that band needs. `regions`: the regions [`RouteStats::coverage`]
/// is split by. `keep_tags`: tags to return apart.
pub fn read(
    pbf: &Pbf,
    wl: &Whitelist,
    marked: &mut Option<NodeSet>,
    band: Band,
    regions: &[(String, Poly)],
    keep_tags: Option<&KeepTags>,
) -> Result<OsmData> {
    let ctx = Ctx::new(wl, band, regions, keep_tags)?;
    let progress = |what: &str| eprintln!("    {what}: peak RSS {:.0} MiB", peak_rss_mib());
    let bbox = header_bbox(pbf.path())?;
    let mut routes = RouteStats::default();
    let add_coverage = |b: &mut Block, routes: &mut RouteStats| {
        let roads = &wl.layers[ctx.roads_layer];
        for ((region, class, src), n) in b.route_coverage.drain() {
            let class = roads.classes[class as usize].name.clone();
            routes
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
    let mut index = Rels::default();
    let mut route_ids: HashMap<(Box<str>, Box<str>), u32> = HashMap::new();
    for mut b in blocks(pbf, "relations", Kinds::RELATIONS, |block, out| {
        relations_block(&ctx, block, out)
    })? {
        rels.append(&mut b.rels);
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
            routes.relations += 1;
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
        .map(|(id, _)| *id)
        .collect();
    let way_pass = |b: &PrimitiveBlock, out: &mut Block| {
        ways_block(&ctx, b, out, &index);
        anyhow::Ok(())
    };

    let mut features = Vec::new();
    let mut member_coords: HashMap<i64, Vec<P>> = HashMap::new();
    let mut kept_tags: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    let mut collect = |b: &mut Block| {
        features.append(&mut b.features);
        kept_tags.extend(b.kept_tags.drain(..));
        member_coords.extend(b.member_coords.drain(..));
        for (id, n) in b.route_ways.drain() {
            *route_ways.entry(id).or_default() += n;
        }
    };
    // Pass 2: ways, marking the nodes they need. Pass 3: those nodes, stored by
    // rank. Pass 4: the ways again, resolved as they are read. Reading the ways twice
    // is cheaper than holding them, since their refs alone are 8 bytes per vertex.
    let needed = match marked {
        Some(set) => set.clone(),
        None => {
            let needed = NodeSet::new();
            blocks(pbf, "ways (mark)", Kinds::WAYS, |block, out| {
                way_pass(block, out)?;
                for rec in out.way_recs.drain(..) {
                    for &id in &rec.refs {
                        needed.insert(id)?;
                    }
                }
                Ok(())
            })?;
            progress("ways marked");
            *marked = Some(needed.clone());
            needed
        }
    };
    let locs = RankedLocations::new(needed);
    eprintln!(
        "    {} needed nodes, {:.1} GiB of locations",
        locs.len(),
        locs.heap_bytes() as f64 / f64::from(1 << 30)
    );
    for mut b in blocks(pbf, "nodes", Kinds::NODES, |block, out| {
        nodes_block(&ctx, block, out, |id, p| locs.set(id, p));
        out.features.retain(|f| ctx.keeps(f));
        Ok(())
    })? {
        collect(&mut b);
    }
    progress("nodes located");
    for mut b in blocks(pbf, "ways (resolve)", Kinds::WAYS, |block, out| {
        let mut ways = Block::default();
        way_pass(block, &mut ways)?;
        for rec in ways.way_recs {
            resolve_way(&ctx, rec, |id| locs.get(id), out);
        }
        out.features.retain(|f| ctx.keeps(f));
        Ok(())
    })? {
        add_coverage(&mut b, &mut routes);
        collect(&mut b);
    }
    progress("ways resolved");

    // Multipolygons from their member ways.
    let lookup = |members: &[(i64, bool)]| -> Option<Vec<(&[P], bool)>> {
        members
            .iter()
            .map(|(id, inner)| member_coords.get(id).map(|c| (c.as_slice(), *inner)))
            .collect()
    };
    let assemble = |members: &[(i64, bool)]| -> Option<Vec<Vec<P>>> {
        match geom::assemble(&lookup(members)?) {
            Assembly::Ok(rings) => Some(rings),
            Assembly::Broken(rings) => Some(rings).filter(|r| !r.is_empty()),
        }
    };
    for r in &mut rels {
        if !r.kept.is_empty() {
            kept_tags.insert(r.id * 4 + 2, std::mem::take(&mut r.kept));
        }
    }
    // In parallel, for the label points of big areas.
    let rel_features: Vec<Feature> = rels
        .into_par_iter()
        .flat_map_iter(|r| {
            let rings = assemble(&r.members);
            let id = r.id * 4 + 2;
            r.outs
                .into_iter()
                .filter_map(|o| ctx.area_feature(id, o, rings.clone()?))
                .filter(|f| ctx.keeps(f))
                .collect::<Vec<_>>()
        })
        .collect();
    features.extend(rel_features);
    for (id, n) in route_ways {
        let network = &index.routes[id as usize].0;
        *routes.networks.entry(network.to_string()).or_default() += n;
    }
    features.sort_unstable_by_key(|f| (f.layer, f.id, f.class));
    Ok(OsmData {
        bbox,
        features,
        kept_tags,
        routes,
    })
}

fn relations_block(ctx: &Ctx, block: &PrimitiveBlock, out: &mut Block) -> Result<()> {
    let mut tags = Vec::new();
    for el in block.elements() {
        let Element::Relation(r) = el else { continue };
        tags.clear();
        tags.extend(r.tags());
        let ty = tags.iter().find(|(k, _)| *k == "type").map(|(_, v)| *v);
        if ty == Some("route") {
            road_route(ctx, &r, &tags, out)?;
            continue;
        }
        if !matches!(ty, Some("multipolygon" | "boundary")) || !ctx.interesting(&tags) {
            continue;
        }
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
            out.boundary_ways
                .extend(ways()?.into_iter().map(|(id, _)| (id, ci as u16)));
        }
        // Protected areas are often `type=boundary`, so both types are tried as areas.
        let outs = classify(ctx, ElementType::Relation, &tags);
        if !outs.is_empty() {
            out.rels.push(RelRec {
                id: r.id(),
                kept: ctx
                    .keep_tags
                    .map(|k| k.of(&outs, &tags))
                    .unwrap_or_default(),
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
        tags.clear();
        tags.extend(w.tags());
        let mut outs = if ctx.interesting(&tags) {
            classify(ctx, ElementType::Way, &tags)
        } else {
            Vec::new()
        };
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
            kept: ctx
                .keep_tags
                .map(|k| k.of(&outs, &tags))
                .unwrap_or_default(),
            outs,
            member,
            route,
        });
    }
}

/// `node` stores a node location.
fn nodes_block(ctx: &Ctx, block: &PrimitiveBlock, out: &mut Block, node: impl Fn(i64, P)) {
    let mut tags = Vec::new();
    let visit = |out: &mut Block, id: i64, lat: i32, lon: i32, tags: &[(&str, &str)]| {
        let p = geom::project_e7(lat, lon);
        node(id, p);
        if !tags.is_empty() && ctx.interesting(tags) {
            let outs = classify(ctx, ElementType::Node, tags);
            if let Some(k) = ctx.keep_tags {
                let kept = k.of(&outs, tags);
                if !kept.is_empty() {
                    out.kept_tags.push((id * 4, kept));
                }
            }
            for o in outs {
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
    };
    for el in block.elements() {
        tags.clear();
        match el {
            Element::DenseNode(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
            }
            Element::Node(n) => {
                tags.extend(n.tags());
                visit(out, n.id(), n.decimicro_lat(), n.decimicro_lon(), &tags);
            }
            _ => {}
        }
    }
}

fn resolve_way(ctx: &Ctx, rec: WayRec, get: impl Fn(i64) -> Option<P>, out: &mut Block) {
    let coords: Vec<P> = rec.refs.into_iter().filter_map(get).collect();
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
    if !rec.kept.is_empty() {
        out.kept_tags.push((rec.id * 4 + 1, rec.kept));
    }
    let ring = geom::closed_way_ring(&coords, true);
    let is_label = |o: &Out| ctx.labels.is_some_and(|(l, _)| l == o.layer);
    for o in rec.outs {
        let id = rec.id * 4 + 1;
        let g = match o.kind {
            GeometryKind::Line if coords.len() >= 2 => Geometry::Lines(vec![coords.clone()]),
            GeometryKind::Area | GeometryKind::Point if ring.is_some() => {
                let rings = vec![ring.clone().unwrap_or_default()];
                if let Some(f) = ctx.area_feature(id, o, rings) {
                    out.features.push(f);
                }
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
    if rec.member {
        out.member_coords.push((rec.id, coords));
    }
}
