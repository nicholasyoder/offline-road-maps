//! Turns a decoded tile into vertex data, once, on a loader thread.
//!
//! Everything stays in tile-local units (−64…4159 fits an `i16`); a per-tile transform
//! places it on screen, so a tile is never re-tessellated when the zoom changes.
//! Polygons are tessellated with `lyon` (even-odd). Lines become one instance per segment,
//! which the vertex shader widens to a screen-width quad with round ends (render.rs).
//! Within each pass the features are sorted by their class's drawing order, so a pass is one
//! contiguous range per tile.

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use lyon_tessellation::geom::point;
use lyon_tessellation::path::Path;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use omp_core::tile::GeomType;
use omp_core::tiles::Tile;

use crate::style::{FILL_PASSES, LINE_PASSES, Pass, Style};

/// A polygon vertex: tile-local position and style ID.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [i16; 2],
    pub style: [u16; 2],
}

/// One line segment, `a` to `b`, with the distance along its line to `a` for dashes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Segment {
    pub a: [i16; 2],
    pub b: [i16; 2],
    pub style: [u16; 2],
    pub dist: f32,
}

#[derive(Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Index ranges, per fill pass.
    pub fills: [Range<u32>; FILL_PASSES.len()],
    pub segments: Vec<Segment>,
    /// Segment ranges, per line pass.
    pub lines: [Range<u32>; LINE_PASSES.len()],
    /// Polygons that `lyon` could not tessellate.
    pub failed: usize,
}

impl Mesh {
    /// Bytes of vertex data, as uploaded.
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(self.vertices.as_slice())
            + std::mem::size_of_val(self.indices.as_slice())
            + std::mem::size_of_val(self.segments.as_slice())
    }
}

fn truthy(v: Option<std::borrow::Cow<'_, str>>) -> bool {
    v.is_some_and(|v| v != "no")
}

fn p16(p: [i32; 2]) -> [i16; 2] {
    p.map(|v| v.clamp(i16::MIN.into(), i16::MAX.into()) as i16)
}

pub fn build(tile: &Tile, style: &Style) -> Mesh {
    // (pass, order, layer index in tile, feature index, style ID)
    let mut items = Vec::new();
    for (li, (layer_id, layer)) in tile.layers.iter().enumerate() {
        let roads = style.roads.filter(|r| r.layer == *layer_id);
        for (fi, f) in layer.features.iter().enumerate() {
            let Some(cs) = style.class(*layer_id, f.class) else {
                continue;
            };
            if f.kind == GeomType::Point || cs.pass.is_fill() != (f.kind == GeomType::Polygon) {
                continue;
            }
            let (mut pass, mut paint) = (cs.pass, cs.paint);
            if let Some(r) = roads {
                if truthy(r.tunnel.and_then(|k| layer.prop(f, k))) {
                    (pass, paint) = (Pass::Tunnels, cs.tunnel.unwrap_or(paint));
                } else if truthy(r.bridge.and_then(|k| layer.prop(f, k))) {
                    (pass, paint) = (Pass::Bridges, cs.bridge.unwrap_or(paint));
                }
            }
            items.push((pass, cs.order, li, fi, paint));
        }
    }
    items.sort_unstable();

    let mut m = Mesh::default();
    let mut tess = FillTessellator::new();
    let mut buf: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::even_odd();
    let mut items = items.into_iter().peekable();
    for pass in FILL_PASSES {
        let start = m.indices.len() as u32;
        while let Some(&(_, _, li, fi, paint)) = items.peek().filter(|it| it.0 == pass) {
            items.next();
            let layer = &tile.layers[li].1;
            let f = &layer.features[fi];
            let mut path = Path::builder();
            for ring in layer.parts(f).filter(|r| r.len() >= 3) {
                path.begin(point(ring[0][0] as f32, ring[0][1] as f32));
                for p in &ring[1..] {
                    path.line_to(point(p[0] as f32, p[1] as f32));
                }
                path.end(true);
            }
            buf.vertices.clear();
            buf.indices.clear();
            let res = tess.tessellate_path(
                &path.build(),
                &options,
                &mut BuffersBuilder::new(&mut buf, |v: FillVertex| v.position().to_array()),
            );
            if res.is_err() {
                m.failed += 1;
                continue;
            }
            let base = m.vertices.len() as u32;
            m.vertices.extend(buf.vertices.iter().map(|v| Vertex {
                pos: p16([v[0].round() as i32, v[1].round() as i32]),
                style: [paint, 0],
            }));
            m.indices.extend(buf.indices.iter().map(|i| base + i));
        }
        m.fills[pass.slot()] = start..m.indices.len() as u32;
    }
    for pass in LINE_PASSES {
        let start = m.segments.len() as u32;
        while let Some(&(_, _, li, fi, paint)) = items.peek().filter(|it| it.0 == pass) {
            items.next();
            let layer = &tile.layers[li].1;
            let f = &layer.features[fi];
            for line in layer.parts(f) {
                let mut dist = 0.0f32;
                for w in line.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if a == b {
                        continue;
                    }
                    m.segments.push(Segment {
                        a: p16(a),
                        b: p16(b),
                        style: [paint, 0],
                        dist,
                    });
                    dist += ((b[0] - a[0]) as f32).hypot((b[1] - a[1]) as f32);
                }
            }
        }
        m.lines[pass.slot()] = start..m.segments.len() as u32;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::StyleFile;
    use omp_core::schema::Schema;
    use omp_core::tile::{Enc, Geom, Layer, encode_layer};
    use omp_core::whitelist::{DEFAULT, Whitelist};

    /// A test feature: class, geometry and props.
    type F<'a> = (&'a str, Geom, Vec<(&'a str, &'a str)>);

    fn tile_with(schema: &Schema, layers: &[(&str, Vec<F>)]) -> Tile {
        let mut out = Vec::new();
        for (name, feats) in layers {
            let (id, l) = schema.layer(name).unwrap();
            let props: Vec<Vec<(u8, Box<str>)>> = feats
                .iter()
                .map(|(_, _, p)| {
                    p.iter()
                        .map(|(k, v)| (l.prop(k).unwrap(), (*v).into()))
                        .collect()
                })
                .collect();
            let enc: Vec<Enc> = feats
                .iter()
                .zip(&props)
                .map(|((class, geom, _), props)| Enc {
                    class: l.class(class).unwrap(),
                    props,
                    geom: geom.clone(),
                })
                .collect();
            out.push((id, Layer::decode(encode_layer(&enc)).unwrap()));
        }
        out.sort_by_key(|(id, _)| *id);
        Tile {
            layers: out,
            bytes: 0,
        }
    }

    #[test]
    fn builds_fills_and_lines_by_pass() {
        let schema = Schema::from_whitelist(&Whitelist::parse(DEFAULT).unwrap());
        let (style, _) = Style::compile(&StyleFile::builtin(), &schema);
        let square =
            |x0: i32, y0: i32, s: i32| vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]];
        // A lake with an island, and a park.
        let lake = Geom::Polygon(vec![
            square(0, 0, 1000),
            square(400, 400, 200).into_iter().rev().collect(),
        ]);
        let road = |pts: Vec<[i32; 2]>| Geom::Lines(vec![pts]);
        let tile = tile_with(
            &schema,
            &[
                (
                    "roads",
                    vec![
                        (
                            "motorway",
                            road(vec![[0, 0], [300, 400], [300, 400], [300, 1000]]),
                            vec![],
                        ),
                        (
                            "residential",
                            road(vec![[0, 0], [10, 0]]),
                            vec![("bridge", "yes")],
                        ),
                        (
                            "residential",
                            road(vec![[0, 0], [10, 0]]),
                            vec![("tunnel", "no")],
                        ),
                    ],
                ),
                ("water", vec![("lake", lake, vec![])]),
                (
                    "landcover",
                    vec![("park", Geom::Polygon(vec![square(-64, -64, 4224)]), vec![])],
                ),
                ("places", vec![("city", Geom::Point([5, 5]), vec![])]),
            ],
        );
        let m = build(&tile, &style);
        assert_eq!(m.failed, 0);
        let fill = |p: Pass| m.fills[p.slot()].clone();
        let line = |p: Pass| m.lines[p.slot()].clone();
        // The park is two triangles; the lake with its hole is more, after it.
        assert_eq!(fill(Pass::Landcover), 0..6);
        assert!(fill(Pass::Water).start == 6 && fill(Pass::Water).len() > 6);
        assert_eq!(m.indices.len() as u32, fill(Pass::Water).end);
        // The repeated point is skipped; "tunnel=no" is a plain road, drawn first.
        assert_eq!(line(Pass::Roads), 0..3);
        assert_eq!(line(Pass::Bridges), 3..4);
        assert_eq!(line(Pass::Tunnels), 0..0);
        let s = &m.segments;
        assert_eq!(s[1].a, [0, 0]);
        assert_eq!(s[2].dist, 500.0);
        assert!(s[1].style != s[3].style);
        assert_eq!(
            m.bytes(),
            m.vertices.len() * 8 + m.indices.len() * 4 + 4 * 16
        );
    }
}
