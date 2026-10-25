//! Route shields and POI icons (phase1-plan.md 1.8), drawn like glyphs: each is a few layers
//! of distance fields in the glyph atlas (text.rs), made from vector outlines on first use
//! and kept, so they scale, get a halo, and need no image library.
//!
//! A shield is its outline in the border colour, the same outline moved [`BORDER`] in, in
//! the body colour, and for some designs a band across the top, then its number. Its width
//! follows the number's, in eighths of an em, so each design is made in a few widths. An
//! icon is a disc in the style's colour with a white pictogram from Maki (CC0,
//! `data/icons/LICENSE.txt`) on it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use omp_core::routes::Shield;
use skrifa::outline::OutlinePen;

use crate::style::{Font, Rgba};
use crate::text::{self, Flatten, GlyphSdf, SDF_EM, Text};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    Shield(Shield),
    Icon(Icon),
}

/// One layer of a shape, as the atlas holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShapeKey {
    pub shape: Shape,
    pub layer: u8,
    /// A shield's width in eighths of an em; 0 for an icon.
    pub width: u8,
}

/// The distance field of a shape's layer, made on first use and kept.
pub fn sdf(key: ShapeKey) -> Option<Arc<GlyphSdf>> {
    static CACHE: OnceLock<Mutex<HashMap<ShapeKey, Option<Arc<GlyphSdf>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(g) = cache.lock().unwrap().get(&key) {
        return g.clone();
    }
    // Made outside the lock, as glyphs are.
    let g = match key.shape {
        Shape::Shield(s) => shield_sdf(s, key.layer, key.width),
        Shape::Icon(i) => icon_sdf(i, key.layer),
    }
    .map(Arc::new);
    cache.lock().unwrap().entry(key).or_insert(g).clone()
}

// --- Shields ---

/// Shield numbers are set in this font.
pub const SHIELD_FONT: Font = Font::Bold;
/// Atlas pixels per em of shields: about 1.6 times their size on screen, half a glyph's, as
/// shields take much room in the atlas and have no fine detail.
const SHIELD_EM: f32 = 16.0;
/// The border's width, in ems.
const BORDER: f32 = 0.09;
/// Numbers wider than this many ems get no shield.
const MAX_TEXT: f32 = 3.2;
/// The least room between a number and the inside of the border, in ems.
const TEXT_ROOM: f32 = 0.1;
/// Shields are made at most this wide, in ems.
const MAX_WIDTH: f32 = 31.0;

/// How a shield design looks. Sizes are in ems of the shield's text.
struct Design {
    /// Draws the outline in a `w` × `h` box, y down.
    outline: fn(&mut Flatten, f32, f32),
    height: f32,
    min_width: f32,
    /// The text's width is multiplied by `stretch`, then `pad` is added on either side:
    /// round and pointed shapes need more room.
    stretch: f32,
    pad: f32,
    border: Rgba,
    body: Rgba,
    /// A band across the top, and its bottom as a share of the height.
    band: Option<(Rgba, f32)>,
    text: Rgba,
    /// The middle of the text, as shares of the width and of the height from the top.
    text_x: f32,
    text_y: f32,
    /// The number's size, as a share of the shield's em.
    text_size: f32,
    /// Text too wide for `min_width` is set smaller, down to this share of `text_size`.
    shrink: f32,
    /// Wider than `min_width`, the outline grows in height too, keeping its shape: true for
    /// every design but those that look right stretched (boxes and ovals).
    keep_aspect: bool,
}

const WHITE: Rgba = [255, 255, 255, 255];
const BLACK: Rgba = [26, 26, 26, 255];
const RED: Rgba = [200, 16, 46, 255];
const BLUE: Rgba = [24, 72, 156, 255];
const GREEN: Rgba = [30, 120, 64, 255];

/// White with a black border and black text, the common look.
const PLAIN: Design = Design {
    outline: rounded_box,
    height: 1.4,
    min_width: 1.4,
    stretch: 1.0,
    pad: 0.25,
    border: BLACK,
    body: WHITE,
    band: None,
    text: BLACK,
    text_x: 0.5,
    text_y: 0.5,
    text_size: 1.0,
    shrink: 1.0,
    keep_aspect: false,
};

/// Shields that keep their shape: a long number is set up to a fifth smaller, then the
/// whole shield grows.
const SHAPED: Design = Design {
    shrink: 0.8,
    keep_aspect: true,
    ..PLAIN
};

impl Design {
    /// The height for a width, in ems.
    fn height(&self, w: f32) -> f32 {
        match self.keep_aspect {
            // From the narrowest width a plate can have, in whole eighths.
            true => self.height * (w / ((self.min_width * 8.0).ceil() / 8.0)).max(1.0),
            false => self.height,
        }
    }
}

fn design(s: Shield) -> Design {
    match s {
        // The digits sit just under the band, where the sides are still straight.
        Shield::Interstate => Design {
            outline: interstate,
            height: 1.7,
            min_width: 1.7,
            pad: 0.35,
            border: WHITE,
            body: BLUE,
            band: Some((RED, 0.24)),
            text: WHITE,
            text_y: 0.53,
            ..SHAPED
        },
        Shield::InterstateBusiness => Design {
            outline: interstate,
            height: 1.7,
            min_width: 1.7,
            pad: 0.35,
            border: WHITE,
            body: GREEN,
            text: WHITE,
            text_y: 0.5,
            ..SHAPED
        },
        Shield::UsRoute => Design {
            outline: us_route,
            height: 1.5,
            min_width: 1.5,
            pad: 0.3,
            text_y: 0.44,
            ..SHAPED
        },
        Shield::Circle => Design {
            outline: ellipse,
            height: 1.5,
            min_width: 1.5,
            stretch: 1.2,
            pad: 0.15,
            ..PLAIN
        },
        Shield::Box => PLAIN,
        Shield::Diamond => Design {
            outline: diamond,
            height: 2.0,
            min_width: 2.0,
            stretch: 1.75,
            pad: 0.1,
            text_size: 0.8,
            shrink: 0.75,
            ..SHAPED
        },
        Shield::Keystone => Design {
            outline: keystone,
            height: 1.5,
            min_width: 1.6,
            pad: 0.35,
            text_y: 0.48,
            ..SHAPED
        },
        Shield::Spade => Design {
            outline: spade,
            height: 1.6,
            min_width: 1.5,
            pad: 0.3,
            border: WHITE,
            body: GREEN,
            text: WHITE,
            text_y: 0.4,
            ..SHAPED
        },
        Shield::Ohio => Design {
            outline: ohio,
            height: 1.6,
            min_width: 1.5,
            stretch: 1.1,
            pad: 0.25,
            // Off centre, towards the wide south-west.
            text_x: 0.43,
            text_y: 0.5,
            shrink: 0.75,
            ..SHAPED
        },
        Shield::County => Design {
            outline: pentagon,
            height: 1.5,
            min_width: 1.45,
            pad: 0.22,
            border: [242, 190, 0, 255],
            body: [22, 84, 168, 255],
            text: [255, 214, 51, 255],
            text_y: 0.4,
            ..SHAPED
        },
        Shield::TransCanada => Design {
            outline: heater,
            height: 1.55,
            min_width: 1.45,
            pad: 0.28,
            border: WHITE,
            body: GREEN,
            text: WHITE,
            text_y: 0.42,
            ..SHAPED
        },
        Shield::Ontario => Design {
            outline: crowned,
            height: 1.75,
            min_width: 1.55,
            pad: 0.28,
            text_y: 0.54,
            ..SHAPED
        },
        Shield::QuebecAutoroute => Design {
            outline: heater,
            height: 1.6,
            min_width: 1.5,
            pad: 0.28,
            border: WHITE,
            body: BLUE,
            band: Some((RED, 0.22)),
            text: WHITE,
            text_y: 0.5,
            ..SHAPED
        },
        Shield::QuebecRoute => Design {
            outline: arched,
            height: 1.55,
            min_width: 1.45,
            pad: 0.25,
            text_y: 0.57,
            ..PLAIN
        },
        Shield::Mexico => Design {
            outline: badge,
            height: 1.55,
            min_width: 1.5,
            pad: 0.28,
            text_y: 0.43,
            ..SHAPED
        },
        Shield::Generic => Design {
            height: 1.35,
            border: [128, 128, 128, 255],
            text: [51, 51, 51, 255],
            ..PLAIN
        },
    }
}

/// One route's shield: its design, its width and its number.
#[derive(Debug, PartialEq)]
pub struct Plate {
    pub shield: Shield,
    /// In eighths of an em.
    pub width: u8,
    pub text: Text,
    /// The text's size, as a share of the shield's em.
    pub text_scale: f32,
}

impl Plate {
    /// Width and height, in ems.
    pub fn size(&self) -> [f32; 2] {
        let w = f32::from(self.width) / 8.0;
        [w, design(self.shield).height(w)]
    }

    /// The layers drawn, bottom first, with their colours.
    pub fn layers(&self) -> impl Iterator<Item = (ShapeKey, Rgba)> + use<> {
        let d = design(self.shield);
        let (shape, width) = (Shape::Shield(self.shield), self.width);
        let colors = [Some(d.border), Some(d.body), d.band.map(|b| b.0)];
        (0u8..).zip(colors).filter_map(move |(layer, c)| {
            Some((
                ShapeKey {
                    shape,
                    layer,
                    width,
                },
                c?,
            ))
        })
    }

    /// The text's colour, and where its middle is from the top-left corner, in ems.
    pub fn text_at(&self) -> (Rgba, [f32; 2]) {
        let d = design(self.shield);
        let [w, h] = self.size();
        (d.text, [d.text_x * w, d.text_y * h])
    }

    /// Heap and inline bytes, for the tile cache.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.text.bytes() - std::mem::size_of::<Text>()
    }
}

/// The plate for a route with this ref, with its text shaped and its fields made (on a
/// loader thread, like glyphs'). `None` if the ref is too long for a shield or the font
/// lacks one of its characters.
pub fn plate(shield: Shield, reference: &str) -> Option<Plate> {
    let fonts = text::fonts();
    if reference.is_empty() || !fonts.covers(SHIELD_FONT, reference) {
        return None;
    }
    let text = fonts.shape(SHIELD_FONT, reference);
    if text.width > MAX_TEXT {
        return None;
    }
    let d = design(shield);
    let room = (d.min_width - 2.0 * d.pad) / d.stretch;
    let text_scale = d.text_size * (room / (text.width * d.text_size)).clamp(d.shrink, 1.0);
    let tw = text.width * text_scale;
    // The padding gives each design its proportions; then it widens until the number is
    // clear of the border, as pointed and round shapes narrow around it.
    let w = (tw * d.stretch + 2.0 * d.pad).max(d.min_width);
    let mut width = (w * 8.0).ceil() as u8;
    while f32::from(width) / 8.0 < MAX_WIDTH && !fits(&d, f32::from(width) / 8.0, tw, text_scale) {
        width += 1;
    }
    let plate = Plate {
        shield,
        width,
        text,
        text_scale,
    };
    for (key, _) in plate.layers() {
        sdf(key);
    }
    for g in &plate.text.glyphs {
        text::glyph_sdf(SHIELD_FONT, g.id);
    }
    Some(plate)
}

/// Whether a number `tw` ems wide, at `scale` of the em, is [`TEXT_ROOM`] clear of the
/// border of design `d` drawn `w` wide: its box's corners and the middles of its sides are
/// inside the outline, at least the border and that room in.
fn fits(d: &Design, w: f32, tw: f32, scale: f32) -> bool {
    let h = d.height(w);
    // Flattened as it is drawn.
    let mut pen = Flatten::new([SHIELD_EM; 2], [0.0; 2]);
    (d.outline)(&mut pen, w, h);
    pen.close();
    let th = 2.0 * text::MID_CAP * scale;
    let (x0, x1) = (d.text_x * w - tw / 2.0, d.text_x * w + tw / 2.0);
    let (y0, y1) = (d.text_y * h - th / 2.0, d.text_y * h + th / 2.0);
    let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    [
        [x0, y0],
        [x1, y0],
        [x0, y1],
        [x1, y1],
        [xm, y0],
        [xm, y1],
        [x0, ym],
        [x1, ym],
    ]
    .iter()
    .all(|&p| depth(&pen.segs, p.map(|v| v * SHIELD_EM)) >= (BORDER + TEXT_ROOM) * SHIELD_EM)
}

/// How far point `p` is inside the outline `segs` (nonzero winding); negative outside.
fn depth(segs: &[text::Segment], p: [f32; 2]) -> f32 {
    let mut winding = 0;
    let mut d2 = f32::MAX;
    for &[a, b] in segs {
        if (a[1] <= p[1]) != (b[1] <= p[1]) {
            let t = (p[1] - a[1]) / (b[1] - a[1]);
            if a[0] + t * (b[0] - a[0]) < p[0] {
                winding += if b[1] > a[1] { 1 } else { -1 };
            }
        }
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let (qx, qy) = (p[0] - a[0], p[1] - a[1]);
        let t = ((qx * ex + qy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        d2 = d2.min((qx - t * ex).powi(2) + (qy - t * ey).powi(2));
    }
    if winding != 0 { d2.sqrt() } else { -d2.sqrt() }
}

fn shield_sdf(s: Shield, layer: u8, width: u8) -> Option<GlyphSdf> {
    let d = design(s);
    let w = f32::from(width) / 8.0;
    let h = d.height(w);
    let mut pen = Flatten::new([SHIELD_EM; 2], [0.0; 2]);
    (d.outline)(&mut pen, w, h);
    pen.close();
    let inset = if layer == 0 { 0.0 } else { BORDER * SHIELD_EM };
    let clip = match layer {
        2 => Some(d.band?.1 * h * SHIELD_EM),
        _ => None,
    };
    text::distance_field(&pen.segs, SHIELD_EM, inset, clip)
}

fn polygon(pen: &mut Flatten, pts: &[[f32; 2]]) {
    pen.move_to(pts[0][0], pts[0][1]);
    for p in &pts[1..] {
        pen.line_to(p[0], p[1]);
    }
    pen.close();
}

fn rounded_box(pen: &mut Flatten, w: f32, h: f32) {
    let r = 0.18;
    pen.move_to(r, 0.0);
    pen.line_to(w - r, 0.0);
    pen.quad_to(w, 0.0, w, r);
    pen.line_to(w, h - r);
    pen.quad_to(w, h, w - r, h);
    pen.line_to(r, h);
    pen.quad_to(0.0, h, 0.0, h - r);
    pen.line_to(0.0, r);
    pen.quad_to(0.0, 0.0, r, 0.0);
    pen.close();
}

fn ellipse(pen: &mut Flatten, w: f32, h: f32) {
    // Four cubic quarter arcs.
    const K: f32 = 0.5523;
    let (cx, cy, rx, ry) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0);
    pen.move_to(cx + rx, cy);
    pen.curve_to(cx + rx, cy + K * ry, cx + K * rx, cy + ry, cx, cy + ry);
    pen.curve_to(cx - K * rx, cy + ry, cx - rx, cy + K * ry, cx - rx, cy);
    pen.curve_to(cx - rx, cy - K * ry, cx - K * rx, cy - ry, cx, cy - ry);
    pen.curve_to(cx + K * rx, cy - ry, cx + rx, cy - K * ry, cx + rx, cy);
    pen.close();
}

fn diamond(pen: &mut Flatten, w: f32, h: f32) {
    polygon(
        pen,
        &[[w / 2.0, 0.0], [w, h / 2.0], [w / 2.0, h], [0.0, h / 2.0]],
    );
}

/// The sides of a shield from `(·, y)` on the right down to a point at the bottom, and up
/// the left to `(0, y)`.
fn shield_sides(pen: &mut Flatten, w: f32, h: f32, y: f32) {
    pen.line_to(w, y);
    pen.curve_to(w, y + 0.6 * (h - y), 0.72 * w, 0.88 * h, w / 2.0, h);
    pen.curve_to(0.28 * w, 0.88 * h, 0.0, y + 0.6 * (h - y), 0.0, y);
}

/// The Interstate shield: a top that dips in the middle.
fn interstate(pen: &mut Flatten, w: f32, h: f32) {
    pen.move_to(0.0, 0.05 * h);
    pen.quad_to(0.25 * w, -0.03 * h, w / 2.0, 0.1 * h);
    pen.quad_to(0.75 * w, -0.03 * h, w, 0.05 * h);
    shield_sides(pen, w, h, 0.6 * h);
    pen.close();
}

/// The US route shield: a flat top with rounded shoulders.
fn us_route(pen: &mut Flatten, w: f32, h: f32) {
    let s = 0.16 * w.min(1.6 * h);
    pen.move_to(s, 0.0);
    pen.line_to(w - s, 0.0);
    pen.quad_to(w - 0.15 * s, 0.05 * h, w, 0.16 * h);
    shield_sides(pen, w, h, 0.55 * h);
    pen.line_to(0.0, 0.16 * h);
    pen.quad_to(0.15 * s, 0.05 * h, s, 0.0);
    pen.close();
}

/// A heater shield: flat top, straight sides, a point at the bottom.
fn heater(pen: &mut Flatten, w: f32, h: f32) {
    pen.move_to(0.0, 0.0);
    pen.line_to(w, 0.0);
    shield_sides(pen, w, h, 0.62 * h);
    pen.close();
}

/// Ontario's King's Highway shield: a heater shield with a crown on top.
fn crowned(pen: &mut Flatten, w: f32, h: f32) {
    pen.move_to(0.0, 0.26 * h);
    for [x, y] in [
        [0.13, 0.06],
        [0.31, 0.18],
        [0.5, 0.0],
        [0.69, 0.18],
        [0.87, 0.06],
        [1.0, 0.26],
    ] {
        pen.line_to(x * w, y * h);
    }
    shield_sides(pen, w, h, 0.66 * h);
    pen.close();
}

/// Pennsylvania's keystone: notched top corners, sides narrowing downwards.
fn keystone(pen: &mut Flatten, w: f32, h: f32) {
    let n = 0.08 * w;
    polygon(
        pen,
        &[
            [0.0, 0.14 * h],
            [n, 0.14 * h],
            [n, 0.0],
            [w - n, 0.0],
            [w - n, 0.14 * h],
            [w, 0.14 * h],
            [w - 0.14 * w, h],
            [0.14 * w, h],
        ],
    );
}

/// California's spade: rounded top corners, sides curving to a point.
fn spade(pen: &mut Flatten, w: f32, h: f32) {
    let r = 0.12 * h;
    pen.move_to(r, 0.0);
    pen.line_to(w - r, 0.0);
    pen.quad_to(w, 0.0, w, r);
    shield_sides(pen, w, h, 0.5 * h);
    pen.line_to(0.0, r);
    pen.quad_to(0.0, 0.0, r, 0.0);
    pen.close();
}

/// Ohio's outline, from the state's border in Web Mercator, stretched to the box.
fn ohio(pen: &mut Flatten, w: f32, h: f32) {
    #[rustfmt::skip]
    const OHIO: [[f32; 2]; 19] = [
        [0.000, 0.081], [0.319, 0.073], [0.400, 0.104], [0.493, 0.154], [0.598, 0.159],
        [0.726, 0.139], [0.877, 0.038], [1.000, 0.000], [1.000, 0.386], [0.970, 0.482],
        [0.930, 0.638], [0.819, 0.750], [0.714, 0.797], [0.609, 0.959], [0.512, 1.000],
        [0.423, 0.925], [0.272, 0.950], [0.121, 0.853], [0.000, 0.820],
    ];
    polygon(pen, &OHIO.map(|[x, y]| [x * w, y * h]));
}

/// The county route pentagon: a square top over a point.
fn pentagon(pen: &mut Flatten, w: f32, h: f32) {
    polygon(
        pen,
        &[
            [0.0, 0.0],
            [w, 0.0],
            [w, 0.7 * h],
            [w / 2.0, h],
            [0.0, 0.7 * h],
        ],
    );
}

/// Quebec's route marker: an arched top over a box.
fn arched(pen: &mut Flatten, w: f32, h: f32) {
    let r = 0.12 * h;
    pen.move_to(0.0, 0.2 * h);
    pen.curve_to(0.22 * w, -0.05 * h, 0.78 * w, -0.05 * h, w, 0.2 * h);
    pen.line_to(w, h - r);
    pen.quad_to(w, h, w - r, h);
    pen.line_to(r, h);
    pen.quad_to(0.0, h, 0.0, h - r);
    pen.close();
}

/// Mexico's federal shield: rounded top corners, sides curving in to a point.
fn badge(pen: &mut Flatten, w: f32, h: f32) {
    let r = 0.16 * h;
    pen.move_to(r, 0.0);
    pen.line_to(w - r, 0.0);
    pen.quad_to(w, 0.0, w, r);
    pen.line_to(w, 0.55 * h);
    pen.quad_to(w, 0.8 * h, w / 2.0, h);
    pen.quad_to(0.0, 0.8 * h, 0.0, 0.55 * h);
    pen.line_to(0.0, r);
    pen.quad_to(0.0, 0.0, r, 0.0);
    pen.close();
}

// --- Icons ---

/// A POI icon, by its index in [`ICONS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Icon(u8);

/// The icons by their name in the style, and their Maki SVG.
const ICONS: [(&str, &str); 11] = [
    ("airport", include_str!("../data/icons/airport.svg")),
    ("hospital", include_str!("../data/icons/hospital.svg")),
    ("rest_area", include_str!("../data/icons/picnic-site.svg")),
    ("fuel", include_str!("../data/icons/fuel.svg")),
    (
        "charging_station",
        include_str!("../data/icons/charging-station.svg"),
    ),
    ("campground", include_str!("../data/icons/campsite.svg")),
    ("attraction", include_str!("../data/icons/attraction.svg")),
    ("hotel", include_str!("../data/icons/lodging.svg")),
    ("restaurant", include_str!("../data/icons/restaurant.svg")),
    ("fast_food", include_str!("../data/icons/fast-food.svg")),
    ("parking", include_str!("../data/icons/parking.svg")),
];

/// Maki's view box is 15 units square.
const MAKI: f32 = 15.0;
/// The pictogram's size on its disc, as a share of the disc's diameter.
const PICTOGRAM: f32 = 0.64;
/// An icon's layers: the disc and the pictogram.
pub const ICON_LAYERS: u8 = 2;

impl Icon {
    pub fn named(name: &str) -> Option<Self> {
        ICONS
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| Self(i as u8))
    }

    pub fn names() -> impl Iterator<Item = &'static str> {
        ICONS.iter().map(|(n, _)| *n)
    }

    pub fn key(self, layer: u8) -> ShapeKey {
        ShapeKey {
            shape: Shape::Icon(self),
            layer,
            width: 0,
        }
    }
}

/// An icon's layer, in a box one em square: the em is the disc's diameter.
fn icon_sdf(icon: Icon, layer: u8) -> Option<GlyphSdf> {
    let mut pen = Flatten::new([SDF_EM; 2], [0.0; 2]);
    if layer == 0 {
        ellipse(&mut pen, 1.0, 1.0);
    } else {
        let k = SDF_EM * PICTOGRAM / MAKI;
        let at = SDF_EM * (1.0 - PICTOGRAM) / 2.0;
        pen.set_transform([k; 2], [at; 2]);
        for d in path_data(ICONS[icon.0 as usize].1) {
            if let Err(e) = svg_path(&d, &mut pen) {
                debug_assert!(false, "icon {}: {e}", ICONS[icon.0 as usize].0);
                return None;
            }
        }
        pen.close();
    }
    text::distance_field(&pen.segs, SDF_EM, 0.0, None)
}

/// The `d` attributes of an SVG file's paths, with character references for whitespace
/// replaced by spaces.
fn path_data(svg: &str) -> Vec<String> {
    svg.split(" d=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .map(|d| d.replace("&#xA;", " ").replace("&#x9;", " "))
        .collect()
}

/// Draws SVG path data: the commands M, L, H, V, C, S, Q, T and Z, absolute and relative.
fn svg_path(d: &str, pen: &mut impl OutlinePen) -> Result<(), String> {
    let mut t = Tokens {
        s: d.as_bytes(),
        i: 0,
    };
    let (mut cur, mut start) = ([0.0f32; 2], [0.0f32; 2]);
    // The last control point of the previous command, for S and T.
    let (mut cubic, mut quad): (Option<[f32; 2]>, Option<[f32; 2]>) = (None, None);
    let mut cmd: Option<u8> = None;
    loop {
        match t.command() {
            Some(c) => cmd = Some(c),
            None if t.at_end() => return Ok(()),
            None => {}
        }
        let c = cmd.ok_or("numbers without a command")?;
        let base = if c.is_ascii_lowercase() {
            cur
        } else {
            [0.0; 2]
        };
        let point = |t: &mut Tokens| -> Result<[f32; 2], String> {
            Ok([t.number()? + base[0], t.number()? + base[1]])
        };
        let reflect = |c: Option<[f32; 2]>, cur: [f32; 2]| {
            c.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]])
        };
        let (mut next_cubic, mut next_quad) = (None, None);
        match c.to_ascii_uppercase() {
            b'M' => {
                cur = point(&mut t)?;
                start = cur;
                pen.move_to(cur[0], cur[1]);
                // Further pairs are lines.
                cmd = Some(if c == b'm' { b'l' } else { b'L' });
            }
            b'L' => {
                cur = point(&mut t)?;
                pen.line_to(cur[0], cur[1]);
            }
            b'H' => {
                cur[0] = t.number()? + base[0];
                pen.line_to(cur[0], cur[1]);
            }
            b'V' => {
                cur[1] = t.number()? + base[1];
                pen.line_to(cur[0], cur[1]);
            }
            b'C' | b'S' => {
                let c1 = match c.to_ascii_uppercase() {
                    b'C' => point(&mut t)?,
                    _ => reflect(cubic, cur),
                };
                let c2 = point(&mut t)?;
                cur = point(&mut t)?;
                pen.curve_to(c1[0], c1[1], c2[0], c2[1], cur[0], cur[1]);
                next_cubic = Some(c2);
            }
            b'Q' | b'T' => {
                let c1 = match c.to_ascii_uppercase() {
                    b'Q' => point(&mut t)?,
                    _ => reflect(quad, cur),
                };
                cur = point(&mut t)?;
                pen.quad_to(c1[0], c1[1], cur[0], cur[1]);
                next_quad = Some(c1);
            }
            b'Z' => {
                pen.close();
                cur = start;
                cmd = None;
            }
            _ => return Err(format!("unsupported path command {:?}", c as char)),
        }
        (cubic, quad) = (next_cubic, next_quad);
    }
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn skip(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == b',')
        {
            self.i += 1;
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip();
        self.i >= self.s.len()
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        (c.is_ascii_alphabetic() && c != b'e' && c != b'E').then(|| {
            self.i += 1;
            c
        })
    }

    /// A number such as `-1.5`, `.5` or `1e-3`; `1.5.5` is two numbers.
    fn number(&mut self) -> Result<f32, String> {
        self.skip();
        let start = self.i;
        let digits = |t: &mut Self| {
            while t.s.get(t.i).is_some_and(u8::is_ascii_digit) {
                t.i += 1;
            }
        };
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        digits(self);
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            digits(self);
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
                self.i += 1;
            }
            digits(self);
        }
        let s = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        s.parse()
            .map_err(|_| format!("expected a number at byte {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::SDF_EDGE;

    /// The field's value at a point given in ems from the shape's origin.
    fn value(g: &GlyphSdf, x: f32, y: f32) -> u8 {
        let i = ((x * g.em - g.left) as usize).min(usize::from(g.w) - 1);
        let j = ((y * g.em - g.top) as usize).min(usize::from(g.h) - 1);
        g.data[j * usize::from(g.w) + i]
    }
    const INSIDE: u8 = (SDF_EDGE * 255.0) as u8;

    #[test]
    fn every_icon_parses_and_has_ink_on_its_disc() {
        assert_eq!(Icon::names().count(), ICONS.len());
        for name in Icon::names() {
            let icon = Icon::named(name).unwrap();
            let mut pen = Flatten::new([1.0; 2], [0.0; 2]);
            for d in path_data(ICONS[icon.0 as usize].1) {
                svg_path(&d, &mut pen).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            // Inside Maki's 15-unit box.
            assert!(
                pen.segs.len() > 4
                    && pen
                        .segs
                        .iter()
                        .flatten()
                        .all(|p| p.iter().all(|v| (-0.01..=MAKI + 0.01).contains(v))),
                "{name}"
            );
            let disc = sdf(icon.key(0)).unwrap();
            assert!(value(&disc, 0.5, 0.5) > INSIDE && value(&disc, 0.02, 0.02) < INSIDE);
            let ink = sdf(icon.key(1)).unwrap();
            assert!(ink.data.iter().any(|&v| v > INSIDE), "{name}");
        }
        assert_eq!(Icon::named("no such icon"), None);
    }

    const ALL: [Shield; 16] = [
        Shield::Interstate,
        Shield::InterstateBusiness,
        Shield::UsRoute,
        Shield::Circle,
        Shield::Box,
        Shield::Diamond,
        Shield::Keystone,
        Shield::Spade,
        Shield::Ohio,
        Shield::County,
        Shield::TransCanada,
        Shield::Ontario,
        Shield::QuebecAutoroute,
        Shield::QuebecRoute,
        Shield::Mexico,
        Shield::Generic,
    ];

    #[test]
    fn numbers_fit_inside_every_shield_body() {
        // The digits' box: cap height around the text's middle, with a little room.
        let cap = 2.0 * text::MID_CAP;
        // Less than TEXT_ROOM: the field is sampled a pixel at a time.
        let margin = 0.03;
        let mut bad = Vec::new();
        for s in ALL {
            for number in ["5", "75", "275"] {
                let p = plate(s, number).unwrap();
                let [w, _] = p.size();
                let (_, [cx, cy]) = p.text_at();
                let (tw, th) = (p.text.width * p.text_scale, cap * p.text_scale);
                let body = sdf(p.layers().nth(1).unwrap().0).unwrap();
                let (x0, x1) = (cx - tw / 2.0 - margin, cx + tw / 2.0 + margin);
                let (y0, y1) = (cy - th / 2.0 - margin, cy + th / 2.0 + margin);
                for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                    if value(&body, x, y) <= INSIDE {
                        bad.push(format!("{s:?} {number}: ({x:.2}, {y:.2}) in {w:.2} wide"));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "outside the body:\n{}", bad.join("\n"));
    }

    #[test]
    #[ignore = "prints the shield sizes, to judge proportions"]
    fn print_sizes() {
        for s in ALL {
            let sizes: Vec<String> = ["5", "75", "275"]
                .iter()
                .map(|n| {
                    let p = plate(s, n).unwrap();
                    let [w, h] = p.size();
                    format!("{w:.2}×{h:.2}")
                })
                .collect();
            println!("{s:?}: {}", sizes.join("  "));
        }
    }

    #[test]
    fn ohio_keeps_its_shape_for_long_numbers() {
        let two = plate(Shield::Ohio, "33").unwrap();
        let three = plate(Shield::Ohio, "315").unwrap();
        assert!(two.text_scale < 1.0 && three.text_scale == 0.75);
        let ([w2, h2], [w3, h3]) = (two.size(), three.size());
        assert!(
            w3 > w2 && (w3 / h3 - w2 / h2).abs() < 0.01,
            "{w2}×{h2}, {w3}×{h3}"
        );
    }

    #[test]
    fn svg_paths_handle_relative_and_smooth_commands() {
        let mut a = Flatten::new([1.0; 2], [0.0; 2]);
        svg_path("M1,1 L5,1 L5,5 Z", &mut a).unwrap();
        let mut b = Flatten::new([1.0; 2], [0.0; 2]);
        svg_path("m1 1h4v4z", &mut b).unwrap();
        assert_eq!(a.segs, b.segs);
        // Implicit lines after a move, numbers run together.
        let mut c = Flatten::new([1.0; 2], [0.0; 2]);
        svg_path("M1-1.5.5 2", &mut c).unwrap();
        assert_eq!(c.segs[0], [[1.0, -1.5], [0.5, 2.0]]);
        let mut d = Flatten::new([1.0; 2], [0.0; 2]);
        svg_path("M0 0c1 0 2 1 2 2s-1 2-2 2", &mut d).unwrap();
        let end = d.segs.iter().map(|s| s[1]).find(|p| p[1] > 3.99).unwrap();
        assert!((end[0] - 0.0).abs() < 1e-4, "{end:?}");
        assert!(svg_path("M0 0 A1 1 0 0 1 2 2", &mut d).is_err());
    }

    #[test]
    fn shields_fit_their_numbers_and_draw_their_layers() {
        let one = plate(Shield::Interstate, "5").unwrap();
        let three = plate(Shield::Interstate, "275").unwrap();
        assert_eq!(one.size(), [1.75, 1.7]);
        // Three digits: smaller, in a larger shield of the same shape.
        let ([w1, h1], [w3, h3]) = (one.size(), three.size());
        assert!(
            w3 > w1 && h3 > h1 && (w3 / h3 - w1 / h1).abs() < 0.05,
            "{w3}×{h3}"
        );
        assert!(three.text_scale < one.text_scale);
        // Border, body and the red band, in their colours.
        let layers: Vec<_> = three.layers().collect();
        assert_eq!(
            layers.iter().map(|l| l.1).collect::<Vec<_>>(),
            [WHITE, BLUE, RED]
        );
        let [w, h] = three.size();
        let border = sdf(layers[0].0).unwrap();
        let body = sdf(layers[1].0).unwrap();
        let band = sdf(layers[2].0).unwrap();
        // The body is inside the border; the band covers only the top.
        assert!(value(&border, w / 2.0, 0.5 * h) > INSIDE);
        assert!(value(&border, 0.03, 0.4 * h) > INSIDE);
        assert!(value(&body, 0.03, 0.4 * h) < INSIDE);
        assert!(value(&band, w / 2.0, 0.2 * h) > INSIDE);
        assert!(value(&band, w / 2.0, 0.5 * h) < INSIDE);
        // Too long, or a character the font lacks.
        assert!(plate(Shield::Box, "Business Loop").is_none());
        assert!(plate(Shield::Box, "").is_none());
        assert!(plate(Shield::Generic, "ᐃ").is_none());
        // Every design draws.
        for s in ALL {
            let p = plate(s, "138").unwrap();
            let [w, h] = p.size();
            for (key, _) in p.layers() {
                let g = sdf(key).unwrap();
                // Bands are at the top.
                let y = if key.layer == 2 { 0.22 } else { 0.6 };
                assert!(value(&g, w / 2.0, y * h) > INSIDE, "{s:?} {}", key.layer);
            }
        }
    }
}
