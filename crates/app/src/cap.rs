//! `--render --cap`: shows what a pack would look like with a layer stored only up to some
//! zoom (phase 1 step 1.5c, experiment B), without rebuilding it. Above the cap the tile's
//! own features of that layer are dropped and its ancestor's at the cap zoom are put in
//! instead, scaled into the tile's grid; the tile's scissor clips them to its square.

use std::io;

use omp_core::schema::Schema;
use omp_core::tile::{EXTENT_BITS, Enc, Geom, GeomType, Layer, encode_layer};
use omp_core::tiles::{Tile, TileReader};

use crate::tiles::TileId;

/// A feature taken out of a layer: class, props and geometry.
type Feat = (u16, Vec<(u8, Box<str>)>, Geom);

/// One `--cap` entry: `landcover=13` caps the whole layer, `water-areas=13` only its polygons.
#[derive(Clone, Copy, Debug)]
pub struct Cap {
    pub layer: u8,
    pub areas_only: bool,
    pub zoom: u8,
}

/// Parses `name[-areas]=z,…` against the pack's layers.
pub fn parse(spec: &str, schema: &Schema) -> Result<Vec<Cap>, String> {
    spec.split(',')
        .map(|item| {
            let (name, z) = item
                .split_once('=')
                .ok_or_else(|| format!("--cap wants layer=zoom, got {item}"))?;
            let zoom = z.parse().map_err(|_| format!("bad zoom in --cap {item}"))?;
            let (name, areas_only) = match name.strip_suffix("-areas") {
                Some(n) => (n, true),
                None => (name, false),
            };
            let (layer, _) = schema
                .layer(name)
                .ok_or_else(|| format!("no layer {name:?} in the pack"))?;
            Ok(Cap {
                layer,
                areas_only,
                zoom,
            })
        })
        .collect()
}

/// `tile` (`id`) as a pack with `caps` would give it.
pub fn apply(reader: &TileReader, id: TileId, mut tile: Tile, caps: &[Cap]) -> io::Result<Tile> {
    for cap in caps.iter().filter(|c| id.z > c.zoom) {
        let d = id.z - cap.zoom;
        let anc = TileId::new(cap.zoom, id.x >> d, id.y >> d);
        let keep = |kind: GeomType| cap.areas_only && kind != GeomType::Polygon;
        let mut feats: Vec<Feat> = Vec::new();
        if let Some(own) = tile.layer(cap.layer) {
            collect(own, |f| keep(f.kind), |g| g, &mut feats);
        }
        // The nearest stored ancestor at or above the cap zoom.
        let mut at = Some(anc);
        let mut found = None;
        while let Some(a) = at {
            if let Some(t) = reader.tile(a.z, a.x, a.y)? {
                found = Some((a, t));
                break;
            }
            at = a.parent();
        }
        if let Some((a, t)) = found
            && let Some(l) = t.layer(cap.layer)
        {
            let d = id.z - a.z;
            let ext = 1i32 << EXTENT_BITS;
            let ox = (id.x - (a.x << d)) as i32 * ext;
            let oy = (id.y - (a.y << d)) as i32 * ext;
            let map = |p: [i32; 2]| [(p[0] << d) - ox, (p[1] << d) - oy];
            let scale = |g: Geom| match g {
                Geom::Point(p) => Geom::Point(map(p)),
                Geom::Lines(ls) => Geom::Lines(
                    ls.into_iter()
                        .map(|l| l.into_iter().map(map).collect())
                        .collect(),
                ),
                Geom::Polygon(rs) => Geom::Polygon(
                    rs.into_iter()
                        .map(|r| r.into_iter().map(map).collect())
                        .collect(),
                ),
            };
            collect(l, |f| !keep(f.kind), scale, &mut feats);
        }
        tile.layers.retain(|(l, _)| *l != cap.layer);
        if !feats.is_empty() {
            let enc: Vec<Enc> = feats
                .iter()
                .map(|(class, props, geom)| Enc {
                    class: *class,
                    props,
                    geom: geom.clone(),
                })
                .collect();
            tile.layers
                .push((cap.layer, Layer::decode(encode_layer(&enc))?));
            tile.layers.sort_by_key(|(l, _)| *l);
        }
    }
    Ok(tile)
}

fn collect(
    layer: &Layer,
    want: impl Fn(&omp_core::tile::Feature) -> bool,
    geom: impl Fn(Geom) -> Geom,
    out: &mut Vec<Feat>,
) {
    for f in layer.features.iter().filter(|f| want(f)) {
        let props = layer
            .props(f)
            .iter()
            .map(|&(k, _)| (k, layer.prop(f, k).unwrap_or_default().into()))
            .collect();
        out.push((f.class, props, geom(layer.geom(f))));
    }
}
