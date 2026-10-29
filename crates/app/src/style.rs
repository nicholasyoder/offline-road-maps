//! What each layer and class looks like: a TOML style file (phase1-plan.md 1.6).
//!
//! The default style (`data/style.toml`) is compiled in; a `style.toml` next to the
//! executable, or `--style <file>`, replaces it. [`StyleFile`] is the parsed and checked
//! file, independent of any pack. Its rules name layers and classes, and [`Style::compile`]
//! resolves them through the pack's `SCHM`, so a class the pack does not have is skipped
//! with a warning and one the style does not name is not drawn.

use std::path::Path;

use omp_core::schema::Schema;
use serde::Deserialize;

use crate::shapes::Icon;

/// The built-in style.
pub const DEFAULT: &str = include_str!("../data/style.toml");
/// The file next to the executable that replaces the built-in style.
pub const FILE: &str = "style.toml";
/// The style file version this app reads.
const VERSION: u32 = 1;

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
/// Every pass, in [`Pass`] order.
pub const PASSES: usize = 9;
const _: () = assert!(Pass::Bridges as usize + 1 == PASSES);
pub const LINE_PASSES: [Pass; 7] = [
    Pass::WaterLines,
    Pass::Boundaries,
    Pass::Tunnels,
    Pass::Rail,
    Pass::Ferries,
    Pass::Roads,
    Pass::Bridges,
];
/// The passes' names in the style file, in [`Pass`] order.
const PASS_NAMES: [&str; PASSES] = [
    "landcover",
    "water",
    "waterlines",
    "boundaries",
    "tunnels",
    "rail",
    "ferries",
    "roads",
    "bridges",
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

    fn from_name(name: &str) -> Option<Self> {
        let i = PASS_NAMES.iter().position(|&n| n == name)?;
        FILL_PASSES
            .into_iter()
            .chain(LINE_PASSES)
            .find(|&p| p as usize == i)
    }
}

/// An RGBA colour, sRGB bytes with straight alpha.
pub type Rgba = [u8; 4];

/// A value that changes with zoom: linear between the stops, constant outside them.
#[derive(Clone, Debug, PartialEq)]
pub struct Stops(pub Vec<(f32, f32)>);

impl Stops {
    pub fn zero() -> Self {
        Self(vec![(0.0, 0.0)])
    }

    pub fn at(&self, zoom: f32) -> f32 {
        let s = &self.0;
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
#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    pub color: Rgba,
    pub width: Stops,
    /// Total width of the casing beyond the line, which is drawn under it; zero for none.
    pub casing_width: Stops,
    pub casing: Rgba,
    /// Dash and gap lengths in points, for the line and for its casing; zero for solid.
    pub dash: [f32; 2],
    pub casing_dash: [f32; 2],
    /// Drawn from `minzoom` up to, not including, `maxzoom`.
    pub minzoom: f32,
    pub maxzoom: f32,
}

impl Paint {
    fn fill(color: Rgba) -> Self {
        Self {
            color,
            width: Stops::zero(),
            casing_width: Stops::zero(),
            casing: [0; 4],
            dash: [0.0; 2],
            casing_dash: [0.0; 2],
            minzoom: 0.0,
            maxzoom: f32::INFINITY,
        }
    }

    pub fn shown_at(&self, zoom: f32) -> bool {
        (self.minzoom..self.maxzoom).contains(&zoom)
    }
}

/// Text style for a class's labels (labels.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// The prop the text comes from.
    pub prop: String,
    /// Its key in the layer, set by [`Style::compile`].
    pub key: Option<u8>,
    pub font: Font,
    /// Text size in points.
    pub size: Stops,
    pub color: Rgba,
    pub halo: Rgba,
    /// Halo width in points.
    pub halo_width: f32,
    /// Higher is placed first.
    pub priority: i32,
    pub placement: Placement,
    pub minzoom: f32,
    pub maxzoom: f32,
}

/// In the order of the bundled fonts (text.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Font {
    Regular,
    Italic,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// At the feature's point, with candidate positions around it.
    Point,
    /// Along the line.
    Line,
}

/// What is drawn on top of a class's geometry: its label, POI icon and route shields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Symbols {
    pub label: Option<Label>,
    /// The POI icon, drawn with the label (shapes.rs).
    pub icon: Option<IconStyle>,
    /// Route shields from the `routes` prop.
    pub shields: Option<Shields>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconStyle {
    pub icon: Icon,
    /// The disc's colour.
    pub color: Rgba,
}

/// Route shields along a road (labels.rs, shapes.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Shields {
    /// The `routes` prop's key in the layer, set by [`Style::compile`].
    pub key: Option<u8>,
    /// The size of the shields' numbers, in points.
    pub size: Stops,
    /// Placed with labels, higher first.
    pub priority: i32,
    pub minzoom: f32,
    pub maxzoom: f32,
}

/// One rule of a style file, checked and with its names resolved.
#[derive(Clone, Debug)]
struct Rule {
    layer: String,
    classes: Vec<String>,
    /// `None`: no geometry is drawn.
    draw: Option<Draw>,
    symbols: Symbols,
}

#[derive(Clone, Debug)]
struct Draw {
    pass: Pass,
    paint: Paint,
    /// Roads only: in a tunnel and on a bridge.
    tunnel: Option<Paint>,
    bridge: Option<Paint>,
}

/// A parsed and checked style file.
#[derive(Clone, Debug)]
pub struct StyleFile {
    /// The colour each tile is drawn on.
    land: Rgba,
    rules: Vec<Rule>,
}

impl StyleFile {
    pub fn builtin() -> Self {
        Self::parse(DEFAULT).expect("the built-in style parses")
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let f: file::File = toml::from_str(text).map_err(|e| e.to_string())?;
        file::resolve(f)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
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
    /// Per layer ID, per class ID: how its geometry is drawn.
    pub classes: Vec<Vec<Option<ClassStyle>>>,
    /// Per layer ID, per class ID: its label, icon and shields.
    pub symbols: Vec<Vec<Option<Symbols>>>,
    /// Per style ID. Style ID 0 is the land under each tile.
    pub paints: Vec<Paint>,
    /// Prop keys of the roads layer's `bridge` and `tunnel`.
    pub roads: Option<RoadProps>,
    /// Per pass (`Pass as usize`), the zoom above which it is drawn from the tile at that
    /// zoom: the `maxzoom` of the layers drawn in it (format 0.3).
    pub pass_cap: [Option<u8>; PASSES],
    /// Per layer ID: its `maxzoom` in the pack, if it is capped.
    pub layer_cap: Vec<Option<u8>>,
    /// Per layer ID: the props that rank its labels.
    pub rank_props: Vec<RankProps>,
}

/// Prop keys that order a layer's labels among those of the same priority.
#[derive(Clone, Copy, Debug, Default)]
pub struct RankProps {
    pub population: Option<u8>,
    pub capital: Option<u8>,
    pub area: Option<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct RoadProps {
    pub layer: u8,
    pub bridge: Option<u8>,
    pub tunnel: Option<u8>,
}

/// The GPU style table holds this many entries (render.rs): the style's, then the route's,
/// then the pushpins'.
pub const MAX_PAINTS: usize = 128;
/// The pushpins' paints, the table's last entries, one per [`crate::pins::Colour`].
pub const PIN_PAINTS: usize = MAX_PAINTS - crate::pins::Colour::ALL.len();
/// The route overlay's paints, before the pushpins', one per [`crate::routing::Look`].
pub const ROUTE_PAINTS: usize = PIN_PAINTS - crate::routing::Look::ALL.len();

impl Style {
    /// Resolves `file` against `schema`. Also returns warnings: rules for layers and classes
    /// the pack does not have, and rules past the paint table's size.
    pub fn compile(file: &StyleFile, schema: &Schema) -> (Self, Vec<String>) {
        let mut classes: Vec<Vec<Option<ClassStyle>>> = empty(schema);
        let mut symbols: Vec<Vec<Option<Symbols>>> = empty(schema);
        let mut paints = vec![Paint::fill(file.land)];
        let mut warnings = Vec::new();
        for (order, rule) in file.rules.iter().enumerate() {
            let Some((l, layer)) = schema.layer(&rule.layer) else {
                warnings.push(format!("style: the pack has no layer {}", rule.layer));
                continue;
            };
            let ids: Vec<u16> = rule
                .classes
                .iter()
                .filter_map(|c| {
                    let id = layer.class(c);
                    if id.is_none() {
                        warnings.push(format!("style: the pack has no class {}/{c}", rule.layer));
                    }
                    id
                })
                .collect();
            if ids.is_empty() {
                continue;
            }
            let cs = match &rule.draw {
                Some(d) => {
                    let n = 1 + usize::from(d.tunnel.is_some()) + usize::from(d.bridge.is_some());
                    if paints.len() + n > ROUTE_PAINTS {
                        warnings.push(format!(
                            "style: more than {ROUTE_PAINTS} paints; {}/{} is not drawn",
                            rule.layer, rule.classes[0]
                        ));
                        None
                    } else {
                        let mut add = |p: &Paint| {
                            paints.push(p.clone());
                            (paints.len() - 1) as u16
                        };
                        Some(ClassStyle {
                            pass: d.pass,
                            order: order as u16,
                            paint: add(&d.paint),
                            tunnel: d.tunnel.as_ref().map(&mut add),
                            bridge: d.bridge.as_ref().map(&mut add),
                        })
                    }
                }
                None => None,
            };
            let mut sym = rule.symbols.clone();
            if let Some(label) = &mut sym.label {
                label.key = layer.prop(&label.prop);
                if label.key.is_none() {
                    warnings.push(format!(
                        "style: the pack has no prop {}.{}; {}/{} is not labelled",
                        rule.layer, label.prop, rule.layer, rule.classes[0]
                    ));
                    sym.label = None;
                }
            }
            if let Some(sh) = &mut sym.shields {
                sh.key = layer.prop("routes");
                if sh.key.is_none() {
                    warnings.push(format!(
                        "style: the pack has no prop {}.routes; {}/{} gets no shields",
                        rule.layer, rule.layer, rule.classes[0]
                    ));
                    sym.shields = None;
                }
            }
            let sym = (sym != Symbols::default()).then_some(sym);
            for c in ids {
                let (l, c) = (l as usize, c as usize);
                if cs.is_some() {
                    classes[l][c] = cs;
                }
                if sym.is_some() {
                    symbols[l][c] = sym.clone();
                }
            }
        }
        let pass_cap = pass_caps(schema, &classes);
        let roads = schema.layer("roads").map(|(id, l)| RoadProps {
            layer: id,
            bridge: l.prop("bridge"),
            tunnel: l.prop("tunnel"),
        });
        let style = Self {
            classes,
            symbols,
            paints,
            roads,
            pass_cap,
            layer_cap: schema.layers.iter().map(|l| l.maxzoom).collect(),
            rank_props: schema
                .layers
                .iter()
                .map(|l| RankProps {
                    population: l.prop("population"),
                    capital: l.prop("capital"),
                    area: l.prop("area"),
                })
                .collect(),
        };
        (style, warnings)
    }

    /// The cap zooms below `z` that some pass is drawn from.
    pub fn caps_below(&self, z: u8) -> Vec<u8> {
        let mut v: Vec<u8> = self
            .pass_cap
            .iter()
            .flatten()
            .copied()
            .filter(|&m| m < z)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn symbols(&self, layer: u8, class: u16) -> Option<&Symbols> {
        self.symbols
            .get(layer as usize)?
            .get(class as usize)?
            .as_ref()
    }

    pub fn class(&self, layer: u8, class: u16) -> Option<&ClassStyle> {
        self.classes
            .get(layer as usize)?
            .get(class as usize)?
            .as_ref()
    }
}

/// A `None` per class of each layer.
fn empty<T: Clone>(schema: &Schema) -> Vec<Vec<Option<T>>> {
    schema
        .layers
        .iter()
        .map(|l| vec![None; l.classes.len()])
        .collect()
}

/// A pass is capped when every layer drawn in it has the same `maxzoom`. A pass that mixes
/// capped and uncapped layers (no style does yet) is drawn from each tile's own data, so the
/// capped layers are missing above their cap; it is reported once.
fn pass_caps(schema: &Schema, classes: &[Vec<Option<ClassStyle>>]) -> [Option<u8>; PASSES] {
    let mut layers: [Vec<u8>; PASSES] = Default::default();
    for (l, cs) in classes.iter().enumerate() {
        for c in cs.iter().flatten() {
            let ls = &mut layers[c.pass as usize];
            if !ls.contains(&(l as u8)) {
                ls.push(l as u8);
            }
        }
    }
    layers.map(|ls| {
        let caps: Vec<Option<u8>> = ls
            .iter()
            .map(|&l| schema.layers[l as usize].maxzoom)
            .collect();
        match caps.first() {
            Some(&first) if caps.iter().all(|&c| c == first) => first,
            Some(_) => {
                eprintln!("style: a pass mixes layers with different maxzoom; drawing it uncapped");
                None
            }
            None => None,
        }
    })
}

/// The style file as written, and its checks.
mod file {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct File {
        version: u32,
        land: String,
        #[serde(default)]
        colors: BTreeMap<String, String>,
        #[serde(default)]
        widths: BTreeMap<String, Num>,
        #[serde(default)]
        tunnel: Variant,
        #[serde(default)]
        bridge: Variant,
        #[serde(default, rename = "rule")]
        rules: Vec<RuleFile>,
    }

    /// A number, zoom stops, or a name from `[widths]`.
    #[derive(Clone, Deserialize)]
    #[serde(untagged)]
    enum Num {
        One(f32),
        Stops(Vec<(f32, f32)>),
        Name(String),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Names {
        One(String),
        Many(Vec<String>),
    }

    #[derive(Clone, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Variant {
        /// How far the line's and casing's colours move towards white, 0–1.
        fade: Option<f32>,
        color: Option<String>,
        casing: Option<String>,
        dash: Option<[f32; 2]>,
        casing_dash: Option<[f32; 2]>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RuleFile {
        layer: String,
        class: Names,
        pass: Option<String>,
        color: Option<String>,
        width: Option<Num>,
        casing: Option<String>,
        casing_width: Option<Num>,
        dash: Option<[f32; 2]>,
        casing_dash: Option<[f32; 2]>,
        minzoom: Option<f32>,
        maxzoom: Option<f32>,
        tunnel: Option<Variant>,
        bridge: Option<Variant>,
        label: Option<LabelFile>,
        icon: Option<String>,
        icon_color: Option<String>,
        shields: Option<ShieldsFile>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ShieldsFile {
        size: Option<Num>,
        priority: Option<i32>,
        minzoom: Option<f32>,
        maxzoom: Option<f32>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LabelFile {
        prop: Option<String>,
        font: Option<Font>,
        size: Option<Num>,
        color: Option<String>,
        halo: Option<String>,
        halo_width: Option<f32>,
        priority: Option<i32>,
        placement: Option<Placement>,
        minzoom: Option<f32>,
        maxzoom: Option<f32>,
    }

    struct Lookup<'a> {
        colors: &'a BTreeMap<String, String>,
        widths: &'a BTreeMap<String, Num>,
    }

    impl Lookup<'_> {
        fn color(&self, s: &str) -> Result<Rgba, String> {
            let hex = match s.starts_with('#') {
                true => s,
                false => self
                    .colors
                    .get(s)
                    .ok_or_else(|| format!("unknown colour {s:?}"))?,
            };
            parse_hex(hex).ok_or_else(|| format!("bad colour {s:?}: want #rrggbb or #rrggbbaa"))
        }

        fn stops(&self, n: &Num) -> Result<Stops, String> {
            let s = match n {
                Num::One(v) => vec![(0.0, *v)],
                Num::Stops(s) => s.clone(),
                Num::Name(name) => match self.widths.get(name) {
                    Some(Num::Name(_)) => return Err(format!("width {name:?} names another")),
                    Some(n) => return self.stops(n),
                    None => return Err(format!("unknown width {name:?}")),
                },
            };
            if s.is_empty() {
                return Err("empty list of stops".into());
            }
            if s.windows(2).any(|w| w[0].0 >= w[1].0) {
                return Err("stops must go up in zoom".into());
            }
            if s.iter()
                .any(|&(z, v)| !z.is_finite() || !v.is_finite() || v < 0.0)
            {
                return Err("stops must be finite and not negative".into());
            }
            Ok(Stops(s))
        }
    }

    fn parse_hex(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#').filter(|h| h.is_ascii())?;
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
            8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
            _ => None,
        }
    }

    fn zooms(min: Option<f32>, max: Option<f32>) -> Result<(f32, f32), String> {
        let (min, max) = (min.unwrap_or(0.0), max.unwrap_or(f32::INFINITY));
        if min.is_nan() || max.is_nan() || min >= max {
            return Err("minzoom must be below maxzoom".into());
        }
        Ok((min, max))
    }

    pub fn resolve(f: File) -> Result<StyleFile, String> {
        if f.version != VERSION {
            return Err(format!(
                "style version {} is not supported (this app reads {VERSION})",
                f.version
            ));
        }
        let n = Lookup {
            colors: &f.colors,
            widths: &f.widths,
        };
        let land = n.color(&f.land).map_err(|e| format!("land: {e}"))?;
        let mut rules = Vec::with_capacity(f.rules.len());
        for (i, r) in f.rules.into_iter().enumerate() {
            let classes = match &r.class {
                Names::One(c) => vec![c.clone()],
                Names::Many(cs) => cs.clone(),
            };
            let what = format!(
                "rule {} ({}/{})",
                i + 1,
                r.layer,
                classes.first().map_or("", String::as_str)
            );
            let err = |e: String| format!("{what}: {e}");
            if classes.is_empty() {
                return Err(err("no class".into()));
            }
            let draw = rule_draw(&r, &f.tunnel, &f.bridge, &n).map_err(err)?;
            let label = match r.label {
                Some(l) => Some(label(l, &n).map_err(|e| err(format!("label: {e}")))?),
                None => None,
            };
            let icon = match (&r.icon, &r.icon_color) {
                (Some(name), color) => Some(IconStyle {
                    icon: Icon::named(name).ok_or_else(|| {
                        let names: Vec<&str> = Icon::names().collect();
                        err(format!(
                            "unknown icon {name:?}; icons are {}",
                            names.join(", ")
                        ))
                    })?,
                    color: n
                        .color(color.as_deref().unwrap_or("#6b6b6b"))
                        .map_err(err)?,
                }),
                (None, Some(_)) => return Err(err("icon_color needs an icon".into())),
                (None, None) => None,
            };
            let shields = match r.shields {
                Some(sh) => Some(shields(sh, &n).map_err(|e| err(format!("shields: {e}")))?),
                None => None,
            };
            let symbols = Symbols {
                label,
                icon,
                shields,
            };
            if draw.is_none() && symbols == Symbols::default() {
                return Err(err(
                    "draws nothing: give it a pass, a label or an icon".into()
                ));
            }
            rules.push(Rule {
                layer: r.layer,
                classes,
                draw,
                symbols,
            });
        }
        Ok(StyleFile { land, rules })
    }

    fn rule_draw(
        r: &RuleFile,
        tunnel: &Variant,
        bridge: &Variant,
        n: &Lookup,
    ) -> Result<Option<Draw>, String> {
        let Some(pass_name) = &r.pass else {
            let drawn = r.color.is_some() || r.width.is_some() || r.casing.is_some();
            return match drawn {
                true => Err("a colour or width needs a pass".into()),
                false => Ok(None),
            };
        };
        let pass = Pass::from_name(pass_name).ok_or_else(|| {
            format!(
                "unknown pass {pass_name:?}; passes are {}",
                PASS_NAMES.join(", ")
            )
        })?;
        let color = n.color(r.color.as_deref().ok_or("needs a color")?)?;
        let (minzoom, maxzoom) = zooms(r.minzoom, r.maxzoom)?;
        let mut paint = Paint {
            minzoom,
            maxzoom,
            ..Paint::fill(color)
        };
        if pass.is_fill() {
            if r.width.is_some() || r.casing.is_some() || r.dash.is_some() {
                return Err(format!(
                    "pass {pass_name} fills: it takes no width, casing or dash"
                ));
            }
        } else {
            paint.width = n.stops(r.width.as_ref().ok_or("a line needs a width")?)?;
            if let Some(c) = &r.casing {
                paint.casing = n.color(c)?;
                let w = r
                    .casing_width
                    .as_ref()
                    .ok_or("a casing needs a casing_width")?;
                paint.casing_width = n.stops(w)?;
            } else if r.casing_width.is_some() {
                return Err("casing_width needs a casing colour".into());
            }
            paint.dash = dash(r.dash)?;
            paint.casing_dash = dash(r.casing_dash)?;
        }
        let variant = |global: &Variant, own: &Option<Variant>| {
            let own = own.clone().unwrap_or_default();
            apply(&paint, &merge(global, &own), n)
        };
        let (tunnel, bridge) = match pass {
            Pass::Roads => (
                Some(variant(tunnel, &r.tunnel)?),
                Some(variant(bridge, &r.bridge)?),
            ),
            _ if r.tunnel.is_some() || r.bridge.is_some() => {
                return Err("tunnel and bridge apply to the roads pass only".into());
            }
            _ => (None, None),
        };
        Ok(Some(Draw {
            pass,
            paint,
            tunnel,
            bridge,
        }))
    }

    fn dash(d: Option<[f32; 2]>) -> Result<[f32; 2], String> {
        match d {
            None => Ok([0.0; 2]),
            Some(d) if d.iter().all(|v| v.is_finite() && *v > 0.0) => Ok(d),
            Some(_) => Err("a dash is [dash, gap], both above zero".into()),
        }
    }

    fn merge(global: &Variant, own: &Variant) -> Variant {
        Variant {
            fade: own.fade.or(global.fade),
            color: own.color.clone().or_else(|| global.color.clone()),
            casing: own.casing.clone().or_else(|| global.casing.clone()),
            dash: own.dash.or(global.dash),
            casing_dash: own.casing_dash.or(global.casing_dash),
        }
    }

    /// `base` changed by `v`: first its colours faded, then any colours and dashes it sets.
    fn apply(base: &Paint, v: &Variant, n: &Lookup) -> Result<Paint, String> {
        let mut p = base.clone();
        if let Some(f) = v.fade {
            if !(0.0..=1.0).contains(&f) {
                return Err("fade must be from 0 to 1".into());
            }
            let fade = |c: Rgba| {
                let m = |v: u8| (f32::from(v) + (255.0 - f32::from(v)) * f).round() as u8;
                [m(c[0]), m(c[1]), m(c[2]), c[3]]
            };
            p.color = fade(p.color);
            p.casing = fade(p.casing);
        }
        if let Some(c) = &v.color {
            p.color = n.color(c)?;
        }
        if let Some(c) = &v.casing {
            p.casing = n.color(c)?;
        }
        if v.dash.is_some() {
            p.dash = dash(v.dash)?;
        }
        if v.casing_dash.is_some() {
            p.casing_dash = dash(v.casing_dash)?;
        }
        Ok(p)
    }

    fn shields(s: ShieldsFile, n: &Lookup) -> Result<Shields, String> {
        let (minzoom, maxzoom) = zooms(s.minzoom, s.maxzoom)?;
        Ok(Shields {
            key: None,
            size: n.stops(s.size.as_ref().unwrap_or(&Num::One(10.0)))?,
            priority: s.priority.unwrap_or(0),
            minzoom,
            maxzoom,
        })
    }

    fn label(l: LabelFile, n: &Lookup) -> Result<Label, String> {
        let (minzoom, maxzoom) = zooms(l.minzoom, l.maxzoom)?;
        Ok(Label {
            prop: l.prop.unwrap_or_else(|| "name".into()),
            key: None,
            font: l.font.unwrap_or(Font::Regular),
            size: n.stops(l.size.as_ref().unwrap_or(&Num::One(12.0)))?,
            color: n.color(l.color.as_deref().unwrap_or("#333333"))?,
            halo: n.color(l.halo.as_deref().unwrap_or("#ffffffd9"))?,
            halo_width: l.halo_width.unwrap_or(1.5),
            priority: l.priority.unwrap_or(0),
            placement: l.placement.unwrap_or(Placement::Point),
            minzoom,
            maxzoom,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omp_core::whitelist::{DEFAULT as WHITELIST, Whitelist};

    fn schema() -> Schema {
        Schema::from_whitelist(&Whitelist::parse(WHITELIST).unwrap())
    }

    fn layer_prop(schema: &Schema, layer: &str, prop: &str) -> Option<u8> {
        schema.layer(layer)?.1.prop(prop)
    }

    #[test]
    fn stops_interpolate_and_clamp() {
        let s = Stops(vec![(10.0, 1.0), (12.0, 3.0)]);
        assert_eq!(s.at(5.0), 1.0);
        assert_eq!(s.at(11.0), 2.0);
        assert_eq!(s.at(20.0), 3.0);
        assert_eq!(Stops::zero().at(3.0), 0.0);
    }

    #[test]
    fn the_default_style_covers_every_class_in_the_whitelist() {
        let schema = schema();
        let (style, warnings) = Style::compile(&StyleFile::builtin(), &schema);
        assert!(warnings.is_empty(), "{warnings:?}");
        for (l, layer) in schema.layers.iter().enumerate() {
            let geometry = matches!(
                layer.name.as_str(),
                "roads" | "ferries" | "rail" | "water" | "waterways" | "landcover" | "boundaries"
            );
            let labelled = matches!(layer.name.as_str(), "places" | "labels" | "pois");
            for (c, class) in layer.classes.iter().enumerate() {
                // Classes kept out of tiles need no style.
                let in_tiles = !matches!(
                    class.name.as_str(),
                    "admin_9" | "admin_10" | "isolated_dwelling"
                ) && layer.name != "addresses";
                let name = format!("{}/{}", layer.name, class.name);
                let styled = style.class(l as u8, c as u16).is_some();
                assert_eq!(styled, geometry && in_tiles, "{name}");
                let sym = style.symbols[l][c].as_ref();
                if labelled && in_tiles {
                    assert!(sym.is_some(), "{name} has no label or icon");
                }
                if layer.name == "pois" {
                    assert!(sym.unwrap().icon.is_some(), "{name} has no icon");
                }
            }
        }
        let roads = style.roads.unwrap();
        assert!(roads.bridge.is_some() && roads.tunnel.is_some());
        let (rl, motorway) = schema
            .layer("roads")
            .and_then(|(l, layer)| Some((l, layer.class("motorway")?)))
            .unwrap();
        let sym = style.symbols[rl as usize][motorway as usize]
            .as_ref()
            .unwrap();
        assert_eq!(
            sym.shields.as_ref().unwrap().key,
            layer_prop(&schema, "roads", "routes")
        );
        assert_eq!(sym.label.as_ref().unwrap().placement, Placement::Line);
        assert_eq!(
            sym.label.as_ref().unwrap().key,
            layer_prop(&schema, "roads", "name")
        );
        // The default whitelist caps land cover and water areas at z13, not the waterways.
        assert_eq!(style.pass_cap[Pass::Landcover as usize], Some(13));
        assert_eq!(style.pass_cap[Pass::Water as usize], Some(13));
        assert_eq!(style.pass_cap[Pass::WaterLines as usize], None);
        assert_eq!(style.caps_below(14), vec![13]);
        assert!(style.caps_below(13).is_empty());
        assert!(style.paints.len() <= ROUTE_PAINTS);
    }

    const SMALL: &str = r##"
        version = 1
        land = "#f0f0f0"
        [colors]
        red = "#ff000080"
        [widths]
        main = [[5, 1], [10, 3.5]]
        [tunnel]
        fade = 0.5
        casing_dash = [3, 2]
        [bridge]
        casing = "#000000"
        [[rule]]
        layer = "roads"
        class = ["motorway", "trunk", "no_such_class"]
        pass = "roads"
        color = "red"
        width = "main"
        casing = "#808080"
        casing_width = 2
        minzoom = 4
        bridge = { casing = "#111111" }
        label = { placement = "line", priority = 7 }
        shields = { minzoom = 6, priority = 9 }
        [[rule]]
        layer = "no_such_layer"
        class = "x"
        icon = "fuel"
        icon_color = "red"
    "##;

    #[test]
    fn a_style_file_resolves_names_variants_and_defaults() {
        let f = StyleFile::parse(SMALL).unwrap();
        assert_eq!(f.land, [240, 240, 240, 255]);
        let d = f.rules[0].draw.as_ref().unwrap();
        assert_eq!(d.pass, Pass::Roads);
        assert_eq!(d.paint.color, [255, 0, 0, 128]);
        assert_eq!(d.paint.width.at(7.5), 2.25);
        assert_eq!(d.paint.casing_width.at(20.0), 2.0);
        assert!(!d.paint.shown_at(3.9) && d.paint.shown_at(4.0) && d.paint.shown_at(30.0));
        let t = d.tunnel.as_ref().unwrap();
        assert_eq!(t.color, [255, 128, 128, 128]);
        assert_eq!(t.casing, [192, 192, 192, 255]);
        assert_eq!(t.casing_dash, [3.0, 2.0]);
        assert_eq!(d.bridge.as_ref().unwrap().casing, [17, 17, 17, 255]);
        let l = f.rules[0].symbols.label.as_ref().unwrap();
        assert_eq!(
            (l.prop.as_str(), l.font, l.priority),
            ("name", Font::Regular, 7)
        );
        assert_eq!(l.size.at(10.0), 12.0);
        let sh = f.rules[0].symbols.shields.as_ref().unwrap();
        assert_eq!((sh.minzoom, sh.priority, sh.size.at(9.0)), (6.0, 9, 10.0));
        let icon = f.rules[1].symbols.icon.unwrap();
        assert_eq!(icon.icon, Icon::named("fuel").unwrap());
        assert_eq!(icon.color, [255, 0, 0, 128]);

        let schema = schema();
        let (style, warnings) = Style::compile(&f, &schema);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let (rl, layer) = schema.layer("roads").unwrap();
        let motorway = style.class(rl, layer.class("motorway").unwrap()).unwrap();
        let trunk = style.class(rl, layer.class("trunk").unwrap()).unwrap();
        assert_eq!(motorway.paint, trunk.paint);
        assert!(style.class(rl, layer.class("primary").unwrap()).is_none());
        // Land, the rule's paint, its tunnel and its bridge.
        assert_eq!(style.paints.len(), 4);
    }

    #[test]
    fn style_file_errors_name_the_rule() {
        let bad = |from: &str, to: &str| {
            let text = SMALL.replacen(from, to, 1);
            assert_ne!(text, SMALL, "{from}");
            StyleFile::parse(&text).unwrap_err()
        };
        assert!(bad("version = 1", "version = 2").contains("version 2"));
        let e = bad("color = \"red\"", "color = \"blue\"");
        assert!(
            e.contains("rule 1 (roads/motorway)") && e.contains("\"blue\""),
            "{e}"
        );
        assert!(bad("pass = \"roads\"", "pass = \"road\"").contains("unknown pass"));
        assert!(bad("[[5, 1], [10", "[[10, 1], [5").contains("go up"));
        assert!(bad("#ff000080", "#ff00").contains("bad colour"));
        assert!(bad("minzoom = 4", "minzoom = 4\nmaxzoom = 3").contains("minzoom"));
        assert!(bad("shields = {", "shield = {").contains("unknown field"));
        assert!(bad("minzoom = 6,", "min = 6,").contains("unknown field"));
        assert!(bad("icon = \"fuel\"\n", "").contains("icon_color needs an icon"));
        assert!(bad("icon = \"fuel\"", "icon = \"gas\"").contains("unknown icon \"gas\""));
        assert!(bad("icon = \"fuel\"\n        icon_color = \"red\"", "").contains("draws nothing"));
        assert!(bad("casing = \"#808080\"\n", "").contains("casing colour"));
    }
}
