//! What each layer and class looks like. For now a hard-coded table (phase1-plan.md 1.5);
//! step 1.6 replaces it with a TOML style file.
//!
//! Rules name layers and classes; [`Style::compile`] resolves them through the pack's
//! `SCHM`, so a class the pack does not have is skipped and one the style does not know is
//! not drawn.

use omp_core::schema::Schema;

/// Where a feature is drawn. Polygons fill; lines are drawn as screen-width strokes, with a
/// casing pass under the fill pass where the rule has a casing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pass {
    Landcover,
    Water,
    WaterLines,
    Boundaries,
    Tunnels,
    Rail,
    Ferries,
    Roads,
    Bridges,
}

pub const FILL_PASSES: [Pass; 2] = [Pass::Landcover, Pass::Water];
pub const LINE_PASSES: [Pass; 7] = [
    Pass::WaterLines,
    Pass::Boundaries,
    Pass::Tunnels,
    Pass::Rail,
    Pass::Ferries,
    Pass::Roads,
    Pass::Bridges,
];

impl Pass {
    pub fn is_fill(self) -> bool {
        FILL_PASSES.contains(&self)
    }

    /// Index among the fill passes, or among the line passes.
    pub fn slot(self) -> usize {
        FILL_PASSES
            .iter()
            .position(|&p| p == self)
            .or_else(|| LINE_PASSES.iter().position(|&p| p == self))
            .unwrap_or(0)
    }
}

/// An RGBA colour, sRGB bytes with straight alpha.
pub type Rgba = [u8; 4];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r, g, b, 255]
}

/// A value that changes with zoom: linear between the stops, constant outside them.
#[derive(Clone, Copy, Debug)]
pub struct Stops(pub &'static [(f32, f32)]);

impl Stops {
    pub const ZERO: Stops = Stops(&[(0.0, 0.0)]);

    pub fn at(&self, zoom: f32) -> f32 {
        let s = self.0;
        match s.iter().position(|&(z, _)| z > zoom) {
            None => s.last().map_or(0.0, |p| p.1),
            Some(0) => s[0].1,
            Some(i) => {
                let ((z0, v0), (z1, v1)) = (s[i - 1], s[i]);
                v0 + (v1 - v0) * (zoom - z0) / (z1 - z0)
            }
        }
    }
}

/// How one style ID is drawn. Widths are in points.
#[derive(Clone, Copy, Debug)]
pub struct Paint {
    pub color: Rgba,
    pub width: Stops,
    /// Total width of the casing, which is drawn under the line; zero for none.
    pub casing_width: Stops,
    pub casing: Rgba,
    /// Dash and gap lengths in points, for the line and for its casing; zero for solid.
    pub dash: [f32; 2],
    pub casing_dash: [f32; 2],
}

impl Paint {
    const fn fill(color: Rgba) -> Self {
        Self {
            color,
            width: Stops::ZERO,
            casing_width: Stops::ZERO,
            casing: [0; 4],
            dash: [0.0; 2],
            casing_dash: [0.0; 2],
        }
    }

    const fn line(color: Rgba, width: Stops) -> Self {
        Self {
            width,
            ..Self::fill(color)
        }
    }

    const fn cased(color: Rgba, width: Stops, casing: Rgba, casing_width: Stops) -> Self {
        Self {
            casing,
            casing_width,
            ..Self::line(color, width)
        }
    }

    const fn dashed(self, dash: [f32; 2]) -> Self {
        Self { dash, ..self }
    }

    /// A tunnel: the fill faded towards white, the casing dashed.
    fn tunnel(self) -> Self {
        let fade = |c: Rgba| {
            let m = |v: u8| ((u16::from(v) + 255) / 2) as u8;
            [m(c[0]), m(c[1]), m(c[2]), c[3]]
        };
        Self {
            color: fade(self.color),
            casing: fade(self.casing),
            casing_dash: [3.0, 2.0],
            ..self
        }
    }

    /// A bridge: a dark casing, so it stands out over the roads below.
    fn bridge(self) -> Self {
        Self {
            casing: rgb(90, 90, 90),
            ..self
        }
    }
}

/// The land colour each tile is drawn on.
pub const LAND: Rgba = rgb(242, 239, 233);

struct Rule {
    layer: &'static str,
    class: &'static str,
    pass: Pass,
    paint: Paint,
}

const WATER: Rgba = rgb(166, 202, 232);

/// Road widths, by importance.
#[rustfmt::skip]
const W_MOTORWAY: Stops = Stops(&[(4.0, 0.8), (7.0, 1.6), (10.0, 3.0), (13.0, 6.0), (16.0, 14.0), (18.0, 30.0)]);
#[rustfmt::skip]
const W_TRUNK: Stops = Stops(&[(5.0, 0.7), (7.0, 1.3), (10.0, 2.6), (13.0, 5.5), (16.0, 13.0), (18.0, 28.0)]);
#[rustfmt::skip]
const W_PRIMARY: Stops = Stops(&[(7.0, 0.7), (10.0, 2.0), (13.0, 4.5), (16.0, 12.0), (18.0, 26.0)]);
#[rustfmt::skip]
const W_SECONDARY: Stops = Stops(&[(9.0, 0.7), (11.0, 1.6), (13.0, 3.6), (16.0, 10.0), (18.0, 24.0)]);
#[rustfmt::skip]
const W_TERTIARY: Stops = Stops(&[(10.0, 0.6), (12.0, 1.4), (14.0, 4.0), (16.0, 9.0), (18.0, 22.0)]);
#[rustfmt::skip]
const W_LINK: Stops = Stops(&[(10.0, 0.6), (13.0, 2.0), (16.0, 6.0), (18.0, 14.0)]);
#[rustfmt::skip]
const W_MINOR: Stops = Stops(&[(12.0, 0.5), (13.0, 1.2), (14.0, 2.5), (16.0, 7.0), (18.0, 18.0)]);
#[rustfmt::skip]
const W_SERVICE: Stops = Stops(&[(13.0, 0.4), (14.0, 1.0), (16.0, 3.5), (18.0, 9.0)]);
/// Casings appear from about z8 for main roads and add about a point on each side.
#[rustfmt::skip]
const C_MAIN: Stops = Stops(&[(7.0, 0.0), (8.0, 1.2), (12.0, 2.0), (16.0, 3.0)]);
#[rustfmt::skip]
const C_MINOR: Stops = Stops(&[(12.0, 0.0), (13.0, 1.0), (16.0, 2.0)]);

const MOTORWAY: Paint = Paint::cased(rgb(226, 82, 58), W_MOTORWAY, rgb(150, 40, 28), C_MAIN);
const TRUNK: Paint = Paint::cased(rgb(240, 146, 74), W_TRUNK, rgb(160, 84, 34), C_MAIN);
const PRIMARY: Paint = Paint::cased(rgb(250, 210, 96), W_PRIMARY, rgb(170, 130, 40), C_MAIN);
const SECONDARY: Paint = Paint::cased(rgb(255, 240, 170), W_SECONDARY, rgb(170, 155, 105), C_MAIN);
const TERTIARY: Paint = Paint::cased(rgb(255, 255, 255), W_TERTIARY, rgb(165, 165, 165), C_MINOR);
const MINOR: Paint = Paint::cased(rgb(255, 255, 255), W_MINOR, rgb(185, 185, 185), C_MINOR);
const SERVICE: Paint = Paint::cased(rgb(255, 255, 255), W_SERVICE, rgb(195, 195, 195), C_MINOR);
const fn link(p: Paint) -> Paint {
    Paint {
        width: W_LINK,
        casing_width: C_MINOR,
        ..p
    }
}

const fn r(layer: &'static str, class: &'static str, pass: Pass, paint: Paint) -> Rule {
    Rule {
        layer,
        class,
        pass,
        paint,
    }
}

/// Road classes, least important first: that is their drawing order within a pass.
const ROADS: &[(&str, Paint)] = &[
    ("service_other", SERVICE),
    ("service_drive_through", SERVICE),
    ("service_parking_aisle", SERVICE),
    ("service_driveway", SERVICE),
    ("service_alley", SERVICE),
    ("service", SERVICE),
    ("living_street", MINOR),
    ("residential", MINOR),
    ("unclassified", MINOR),
    ("tertiary_link", link(TERTIARY)),
    ("secondary_link", link(SECONDARY)),
    ("primary_link", link(PRIMARY)),
    ("trunk_link", link(TRUNK)),
    ("motorway_link", link(MOTORWAY)),
    ("tertiary", TERTIARY),
    ("secondary", SECONDARY),
    ("primary", PRIMARY),
    ("trunk", TRUNK),
    ("motorway", MOTORWAY),
];

/// Everything but roads, in drawing order within each pass.
#[rustfmt::skip]
fn rules() -> Vec<Rule> {
    use Pass::*;
    let line = Paint::line;
    vec![
        r("landcover", "farmland", Landcover, Paint::fill(rgb(238, 240, 214))),
        r("landcover", "residential", Landcover, Paint::fill(rgb(232, 228, 224))),
        r("landcover", "commercial", Landcover, Paint::fill(rgb(240, 226, 224))),
        r("landcover", "retail", Landcover, Paint::fill(rgb(240, 222, 220))),
        r("landcover", "industrial", Landcover, Paint::fill(rgb(230, 224, 232))),
        r("landcover", "forest", Landcover, Paint::fill(rgb(208, 228, 192))),
        r("landcover", "wood", Landcover, Paint::fill(rgb(208, 228, 192))),
        r("landcover", "protected_area", Landcover, Paint::fill([150, 200, 140, 70])),
        r("landcover", "park", Landcover, Paint::fill(rgb(196, 230, 178))),
        r("water", "ocean", Water, Paint::fill(WATER)),
        r("water", "lake", Water, Paint::fill(WATER)),
        r("water", "riverbank", Water, Paint::fill(WATER)),
        r("water", "stream", WaterLines, line(WATER, Stops(&[(13.0, 0.6), (16.0, 1.5), (18.0, 3.0)]))),
        r("water", "canal", WaterLines, line(WATER, Stops(&[(10.0, 0.6), (14.0, 2.0), (18.0, 6.0)]))),
        r("water", "river", WaterLines, line(WATER, Stops(&[(8.0, 0.7), (12.0, 1.5), (14.0, 2.5), (18.0, 8.0)]))),
        r("boundaries", "admin_8", Boundaries, line(rgb(196, 180, 196), Stops(&[(11.0, 0.6), (16.0, 1.2)])).dashed([3.0, 2.0])),
        r("boundaries", "admin_6", Boundaries, line(rgb(186, 164, 186), Stops(&[(8.0, 0.6), (14.0, 1.4)])).dashed([5.0, 3.0])),
        r("boundaries", "admin_4", Boundaries, line(rgb(166, 130, 170), Stops(&[(3.0, 0.7), (8.0, 1.4), (14.0, 2.2)])).dashed([8.0, 3.0])),
        r("boundaries", "admin_2", Boundaries, line(rgb(140, 100, 150), Stops(&[(0.0, 0.8), (6.0, 1.6), (12.0, 2.8)]))),
        r("rail", "rail_service", Rail, line(rgb(170, 170, 170), Stops(&[(13.0, 0.6), (16.0, 1.2)]))),
        r("rail", "rail", Rail, line(rgb(150, 150, 150), Stops(&[(9.0, 0.6), (13.0, 1.2), (16.0, 2.0)]))),
        r("ferries", "ferry", Ferries, line(rgb(90, 130, 200), Stops(&[(7.0, 0.7), (12.0, 1.2), (16.0, 1.8)])).dashed([6.0, 4.0])),
    ]
}

/// How one class of one layer is drawn.
#[derive(Clone, Copy, Debug)]
pub struct ClassStyle {
    pub pass: Pass,
    /// Drawing order within the pass: lower first.
    pub order: u16,
    /// Index into [`Style::paints`].
    pub paint: u16,
    /// Roads only: the paint for a tunnel and for a bridge.
    pub tunnel: Option<u16>,
    pub bridge: Option<u16>,
}

/// A style resolved against one pack's schema.
pub struct Style {
    /// Per layer ID, per class ID.
    pub classes: Vec<Vec<Option<ClassStyle>>>,
    /// Per style ID. Style ID 0 is the land under each tile.
    pub paints: Vec<Paint>,
    /// Prop keys of the roads layer's `bridge` and `tunnel`.
    pub roads: Option<RoadProps>,
}

#[derive(Clone, Copy, Debug)]
pub struct RoadProps {
    pub layer: u8,
    pub bridge: Option<u8>,
    pub tunnel: Option<u8>,
}

/// The GPU style table holds this many entries (render.rs).
pub const MAX_PAINTS: usize = 128;

impl Style {
    pub fn compile(schema: &Schema) -> Self {
        let mut classes: Vec<Vec<Option<ClassStyle>>> = schema
            .layers
            .iter()
            .map(|l| vec![None; l.classes.len()])
            .collect();
        let mut paints = vec![Paint::fill(LAND)];
        let mut add = |p: Paint| {
            paints.push(p);
            (paints.len() - 1) as u16
        };
        let mut set = |layer: &str, class: &str, cs: ClassStyle| {
            if let Some((l, layer)) = schema.layer(layer)
                && let Some(c) = layer.class(class)
            {
                classes[l as usize][c as usize] = Some(cs);
            }
        };
        for (order, rule) in rules().into_iter().enumerate() {
            let cs = ClassStyle {
                pass: rule.pass,
                order: order as u16,
                paint: add(rule.paint),
                tunnel: None,
                bridge: None,
            };
            set(rule.layer, rule.class, cs);
        }
        for (order, &(class, paint)) in ROADS.iter().enumerate() {
            let cs = ClassStyle {
                pass: Pass::Roads,
                order: order as u16,
                paint: add(paint),
                tunnel: Some(add(paint.tunnel())),
                bridge: Some(add(paint.bridge())),
            };
            set("roads", class, cs);
        }
        debug_assert!(paints.len() <= MAX_PAINTS);
        paints.truncate(MAX_PAINTS);
        let roads = schema.layer("roads").map(|(id, l)| RoadProps {
            layer: id,
            bridge: l.prop("bridge"),
            tunnel: l.prop("tunnel"),
        });
        Self {
            classes,
            paints,
            roads,
        }
    }

    pub fn class(&self, layer: u8, class: u16) -> Option<&ClassStyle> {
        self.classes
            .get(layer as usize)?
            .get(class as usize)?
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omp_core::whitelist::{DEFAULT, Whitelist};

    #[test]
    fn stops_interpolate_and_clamp() {
        let s = Stops(&[(10.0, 1.0), (12.0, 3.0)]);
        assert_eq!(s.at(5.0), 1.0);
        assert_eq!(s.at(11.0), 2.0);
        assert_eq!(s.at(20.0), 3.0);
        assert_eq!(Stops::ZERO.at(3.0), 0.0);
    }

    #[test]
    fn every_drawn_class_in_the_whitelist_has_a_style() {
        let schema = Schema::from_whitelist(&Whitelist::parse(DEFAULT).unwrap());
        let style = Style::compile(&schema);
        for (l, layer) in schema.layers.iter().enumerate() {
            let drawn = matches!(
                layer.name.as_str(),
                "roads" | "ferries" | "rail" | "water" | "landcover" | "boundaries"
            );
            for (c, class) in layer.classes.iter().enumerate() {
                let styled = style.class(l as u8, c as u16).is_some();
                // Classes kept out of tiles need no style.
                let wanted = drawn && !matches!(class.name.as_str(), "admin_9" | "admin_10");
                assert_eq!(styled, wanted, "{}/{}", layer.name, class.name);
            }
        }
        let roads = style.roads.unwrap();
        assert!(roads.bridge.is_some() && roads.tunnel.is_some());
        assert!(style.paints.len() <= MAX_PAINTS);
    }
}
