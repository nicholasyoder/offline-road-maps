//! Map labels (phase1-plan.md 1.7): what each tile has to label, taken out of the decoded
//! tile on a loader thread, and where the labels go on screen, placed every frame.
//!
//! Placement is greedy. Candidates are sorted by the style's priority, then by size within a
//! priority (population, capital, area), and each is placed if none of its boxes overlaps a
//! placed label's, looked up in a screen grid. Point labels try a few positions around their
//! point; line labels are laid out glyph by glyph along their line, at anchors a fixed
//! distance apart along it, where it bends little, and never upside down. Line labels with
//! the same text (a road's pieces in several tiles, or a long road) are kept [`REPEAT`] apart.
//!
//! A label shown in the last frame gets a priority bonus ([`KEEP`]), so while the map moves
//! labels keep their places instead of trading them with their neighbours; new labels fade
//! in. Anchors are fixed points of the tile data, so they do not move under the map while it
//! pans or zooms within one tile zoom.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use omp_core::tile::{EXTENT_BITS, GeomType, Layer, MAX_ZOOM, int_value};
use omp_core::tiles::Tile;

use crate::camera::Camera;
use crate::render::{GlyphInstance, gpu_color};
use crate::style::{Font, Label, Placement, RankProps, Style};
use crate::text::{self, ATLAS, Atlas, LINE_HEIGHT, MID_CAP, SDF_EM, Text};
use crate::tiles::TileId;

const EXTENT: f32 = (1u32 << EXTENT_BITS) as f32;

/// Point labels wider than this many ems are broken into lines at spaces.
const WRAP: f32 = 8.0;
/// A new label's fade-in, in seconds.
const FADE: f32 = 0.2;
/// The priority bonus of a label shown in the last frame. Style priorities are 5–10 apart,
/// so a label keeps its place against its peers but not against a clearly more important
/// one.
const KEEP: f32 = 6.0;
/// Line labels with the same text are at least this far apart, in points.
const REPEAT: f32 = 400.0;
/// Line label anchors are this many tile units apart along the line at the tile's own zoom
/// (128 points), and closer in an overzoomed or fallback tile, so they stay about as far
/// apart on screen.
const SPACING: f32 = 1024.0;
/// The largest turn between neighbouring glyphs of a line label, and in the whole label,
/// in radians.
const MAX_BEND: f32 = 0.45;
const MAX_TURN: f32 = 1.2;
/// Space kept clear around a label, in points.
const PAD: f32 = 2.0;
/// A label within this many points of where a label with its text was shown in the last
/// frame is the same label.
const SAME: f64 = 3.0;
/// The POI marker until icons come (1.8), drawn as text.
const MARKER: &str = "\u{2022}";
/// The marker's size relative to the label's.
const MARKER_SIZE: f32 = 1.6;

/// Where a label goes, in tile units.
#[derive(Debug, PartialEq)]
pub enum Geom {
    Point([i16; 2]),
    Line(Box<[[i16; 2]]>),
}

#[derive(Debug)]
pub struct TileLabel {
    pub layer: u8,
    pub class: u16,
    pub text: Arc<Text>,
    /// The layer and the text, hashed: a label is matched to the last frame's by it, and
    /// line labels with the same key are kept apart.
    pub key: u64,
    /// Order among labels of the same priority, higher first, 0–16.
    pub rank: f32,
    pub geom: Geom,
}

/// A tile's labels.
#[derive(Debug, Default)]
pub struct TileLabels {
    pub labels: Vec<TileLabel>,
    /// Bytes held, for the tile cache.
    pub bytes: usize,
}

/// Takes the labels out of decoded tile `id`: the features whose class has a label rule
/// shown at a zoom the tile is drawn at, and a name the fonts can draw, shaped. Also makes
/// the glyphs' distance fields, so the UI thread does not have to.
pub fn extract(id: TileId, tile: &Tile, style: &Style) -> TileLabels {
    // A tile stands in for its parent from 1.5 zooms below its own (tiles.rs, `Cover::Child`)
    // and is drawn up to half a zoom above it, or overzoomed. As a fallback for tiles still
    // loading it is drawn higher, without the labels that start there.
    let lo = f32::from(id.z) - 1.5;
    let hi = match id.z {
        MAX_ZOOM => f32::INFINITY,
        z => f32::from(z) + 0.5,
    };
    let fonts = text::fonts();
    let mut out = TileLabels::default();
    // Shaped texts, by font and placement, so a road's pieces share one.
    let mut shaped: HashMap<(Font, Placement), HashMap<String, Arc<Text>>> = HashMap::new();
    for (layer_id, layer) in &tile.layers {
        let rank_props = style.rank_props.get(*layer_id as usize).copied();
        for f in &layer.features {
            let Some(rule) = style.label(*layer_id, f.class) else {
                continue;
            };
            if rule.minzoom >= hi || rule.maxzoom <= lo {
                continue;
            }
            match (rule.placement, f.kind) {
                (Placement::Point, GeomType::Point) | (Placement::Line, GeomType::Lines) => {}
                _ => continue,
            }
            let Some(name) = rule.key.and_then(|k| layer.prop(f, k)) else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let texts = shaped.entry((rule.font, rule.placement)).or_default();
            let text = match texts.get(name) {
                Some(t) => t.clone(),
                None => {
                    if !fonts.covers(rule.font, name) {
                        continue;
                    }
                    let t = match rule.placement {
                        Placement::Point => fonts.shape_wrapped(rule.font, name, WRAP),
                        Placement::Line => fonts.shape(rule.font, name),
                    };
                    for g in &t.glyphs {
                        text::glyph_sdf(rule.font, g.id);
                    }
                    out.bytes += t.bytes();
                    let t = Arc::new(t);
                    texts.insert(name.to_string(), t.clone());
                    t
                }
            };
            let mut h = std::hash::DefaultHasher::new();
            (layer_id, name).hash(&mut h);
            let key = h.finish();
            let rank = rank_props.map_or(0.0, |r| rank(layer, f, r));
            let p16 = |p: [i32; 2]| p.map(|v| v.clamp(i16::MIN.into(), i16::MAX.into()) as i16);
            let mut push = |geom| {
                out.labels.push(TileLabel {
                    layer: *layer_id,
                    class: f.class,
                    text: text.clone(),
                    key,
                    rank,
                    geom,
                })
            };
            match f.kind {
                GeomType::Point => {
                    if let Some(p) = layer.parts(f).next().and_then(|p| p.first()) {
                        push(Geom::Point(p16(*p)));
                    }
                }
                _ => {
                    for line in layer.parts(f).filter(|l| l.len() >= 2) {
                        out.bytes += line.len() * 4;
                        push(Geom::Line(line.iter().map(|&p| p16(p)).collect()));
                    }
                }
            }
        }
    }
    out.bytes += out.labels.len() * std::mem::size_of::<TileLabel>();
    out
}

/// Population (log₁₀), capital and area (`area` is log₂ of square pixels at z14).
fn rank(layer: &Layer, f: &omp_core::tile::Feature, r: RankProps) -> f32 {
    let num = |k: Option<u8>| k.and_then(|k| layer.prop(f, k)).and_then(|v| int_value(&v));
    let mut rank = 0.0;
    if let Some(p) = num(r.population) {
        rank += ((p + 1) as f32).log10();
    }
    if r.capital
        .and_then(|k| layer.prop(f, k))
        .is_some_and(|v| v != "no")
    {
        rank += 2.0;
    }
    if let Some(a) = num(r.area) {
        rank += a as f32 / 4.0;
    }
    rank.clamp(0.0, 15.9)
}

/// Labels from one tile, for [`Placer::place`].
pub struct Source<'a> {
    pub labels: &'a TileLabels,
    /// The tile they are from.
    pub tile: TileId,
    /// Only labels anchored in this tile's square are drawn (tiles.rs, `Plan::labels`).
    pub square: TileId,
    /// Only the layers capped at this zoom.
    pub cap: Option<u8>,
}

/// What [`Placer::place`] made of a frame.
#[derive(Debug, Default)]
pub struct Placed {
    pub glyphs: Vec<GlyphInstance>,
    pub labels: usize,
    pub candidates: usize,
    /// Some labels are still fading in: draw another frame.
    pub fading: bool,
}

#[derive(Clone, Copy)]
struct Shown {
    /// Its anchor, in world coordinates.
    at: [f64; 2],
    opacity: f32,
}

struct Candidate {
    source: u32,
    label: u32,
    /// Anchor in viewport pixels, and in world coordinates.
    at: [f32; 2],
    world: [f64; 2],
    /// A line label's line, as a range of the line buffer, and its distance along it.
    line: Option<(u32, u32, f32)>,
    /// Text size in pixels.
    size: f32,
    order: f32,
    /// Opacity in the last frame, if it was shown.
    shown: Option<f32>,
}

/// A glyph laid out on screen: its pen position and direction.
#[derive(Clone, Copy)]
struct Pen {
    at: [f32; 2],
    dir: [f32; 2],
    id: u16,
    font: Font,
    size: f32,
}

/// Places labels frame after frame, remembering what it showed.
#[derive(Default)]
pub struct Placer {
    pub atlas: Atlas,
    shown: HashMap<u64, Vec<Shown>>,
    last: Option<Instant>,
    grid: Grid,
    // Buffers kept between frames.
    points: Vec<[f32; 2]>,
    dists: Vec<f32>,
    candidates: Vec<Candidate>,
    pens: Vec<Pen>,
    boxes: Vec<[f32; 4]>,
}

impl Placer {
    /// Places the labels of `sources` for `cam`. `now` is the frame's time, for fading in;
    /// `None` shows every label at once (`--render`). `srgb`: the target is sRGB.
    pub fn place(
        &mut self,
        cam: &Camera,
        pixels_per_point: f32,
        style: &Style,
        sources: &[Source],
        srgb: bool,
        now: Option<Instant>,
    ) -> Placed {
        self.atlas.begin_frame();
        let fade = match (now, self.last) {
            (Some(n), Some(l)) => n.saturating_duration_since(l).as_secs_f32() / FADE,
            (Some(_), None) => 0.0,
            (None, _) => f32::INFINITY,
        };
        self.last = now;
        let zoom = cam.zoom as f32;
        let ppp = pixels_per_point;
        let ppp64 = f64::from(ppp);
        let view = [(cam.size[0] * ppp64) as f32, (cam.size[1] * ppp64) as f32];
        let world_px = cam.scale() * ppp64;
        let to_px = |w: [f64; 2]| {
            let p = cam.world_to_screen(w);
            [(p[0] * ppp64) as f32, (p[1] * ppp64) as f32]
        };
        let on_screen =
            |p: [f32; 2]| (0.0..view[0]).contains(&p[0]) && (0.0..view[1]).contains(&p[1]);
        let round_zoom = (cam.zoom + 0.5).floor();

        self.points.clear();
        self.dists.clear();
        self.candidates.clear();
        for (si, s) in sources.iter().enumerate() {
            let [x0, y0, x1, _] = s.tile.world_rect();
            let tile_world = x1 - x0;
            let unit_px = (tile_world / f64::from(EXTENT) * world_px) as f32;
            let origin = to_px([x0, y0]);
            let sq = s.square.world_rect();
            let in_square =
                |w: [f64; 2]| (sq[0]..sq[2]).contains(&w[0]) && (sq[1]..sq[3]).contains(&w[1]);
            let to_world = |p: [f32; 2]| {
                [
                    x0 + f64::from(p[0]) / f64::from(EXTENT) * tile_world,
                    y0 + f64::from(p[1]) / f64::from(EXTENT) * tile_world,
                ]
            };
            let spacing =
                SPACING * unit_px / (round_zoom - f64::from(s.tile.z)).max(0.0).exp2() as f32;
            for (li, l) in s.labels.labels.iter().enumerate() {
                if s.cap.is_some()
                    && style.layer_cap.get(l.layer as usize).copied().flatten() != s.cap
                {
                    continue;
                }
                let Some(rule) = style.label(l.layer, l.class) else {
                    continue;
                };
                if !(rule.minzoom..rule.maxzoom).contains(&zoom) {
                    continue;
                }
                let size = rule.size.at(zoom) * ppp;
                let mut push = |at: [f32; 2], world: [f64; 2], line| {
                    self.candidates.push(Candidate {
                        source: si as u32,
                        label: li as u32,
                        at,
                        world,
                        line,
                        size,
                        order: 0.0,
                        shown: None,
                    })
                };
                match &l.geom {
                    Geom::Point(p) => {
                        let w = to_world([f32::from(p[0]), f32::from(p[1])]);
                        let at = to_px(w);
                        if in_square(w) && on_screen(at) {
                            push(at, w, None);
                        }
                    }
                    Geom::Line(pts) => {
                        let start = self.points.len();
                        let (mut d, mut prev) = (0.0, None);
                        for p in pts.iter() {
                            let q = [
                                origin[0] + f32::from(p[0]) * unit_px,
                                origin[1] + f32::from(p[1]) * unit_px,
                            ];
                            if let Some(r) = prev {
                                d += dist(q, r);
                            }
                            prev = Some(q);
                            self.points.push(q);
                            self.dists.push(d);
                        }
                        let end = self.points.len();
                        let room = l.text.width * size / 2.0 + size;
                        if d < 2.0 * room {
                            self.points.truncate(start);
                            self.dists.truncate(start);
                            continue;
                        }
                        let mut k = 0.5;
                        loop {
                            let a = k * spacing;
                            k += 1.0;
                            if a > d - room {
                                break;
                            }
                            if a < room {
                                continue;
                            }
                            let at = point_at(&self.points[start..end], &self.dists[start..end], a);
                            let w = to_world([
                                (at[0] - origin[0]) / unit_px,
                                (at[1] - origin[1]) / unit_px,
                            ]);
                            if in_square(w) && on_screen(at) {
                                push(at, w, Some((start as u32, end as u32, a)));
                            }
                        }
                    }
                }
            }
        }

        // Order: priority, a bonus for being shown last frame, then rank; ties broken by
        // text and place, so the order does not depend on the order of the tiles.
        let same = SAME * ppp64 / world_px;
        for c in &mut self.candidates {
            let l = &sources[c.source as usize].labels.labels[c.label as usize];
            let rule = style.label(l.layer, l.class).expect("checked above");
            c.shown = self.shown.get(&l.key).and_then(|v| {
                v.iter()
                    .find(|s| {
                        (s.at[0] - c.world[0]).abs() < same && (s.at[1] - c.world[1]).abs() < same
                    })
                    .map(|s| s.opacity)
            });
            let keep = if c.shown.is_some() { KEEP } else { 0.0 };
            c.order = (rule.priority as f32 + keep) * 16.0 + l.rank;
        }
        let key = |c: &Candidate| sources[c.source as usize].labels.labels[c.label as usize].key;
        self.candidates.sort_by(|a, b| {
            b.order
                .total_cmp(&a.order)
                .then_with(|| key(a).cmp(&key(b)))
                .then_with(|| a.world[0].total_cmp(&b.world[0]))
                .then_with(|| a.world[1].total_cmp(&b.world[1]))
        });

        self.grid.reset(view);
        let mut out = Placed {
            candidates: self.candidates.len(),
            ..Default::default()
        };
        let mut shown: HashMap<u64, Vec<Shown>> = HashMap::with_capacity(self.shown.len());
        // Where line labels with each key were placed, for REPEAT.
        let mut lines: HashMap<u64, Vec<[f32; 2]>> = HashMap::new();
        let repeat = REPEAT * ppp;
        let pad = PAD * ppp;
        for c in &self.candidates {
            let l = &sources[c.source as usize].labels.labels[c.label as usize];
            let rule = style.label(l.layer, l.class).expect("checked above");
            self.pens.clear();
            self.boxes.clear();
            let placed = match c.line {
                Some((start, end, d)) => {
                    let near = lines
                        .get(&l.key)
                        .is_some_and(|v| v.iter().any(|&p| dist(p, c.at) < repeat));
                    !near
                        && line_layout(
                            &self.points[start as usize..end as usize],
                            &self.dists[start as usize..end as usize],
                            d,
                            &l.text,
                            c.size,
                            pad,
                            &mut self.pens,
                            &mut self.boxes,
                        )
                        && !self.boxes.iter().any(|b| self.grid.hits(b))
                }
                None => {
                    let icon = style
                        .symbols(l.layer, l.class)
                        .is_some_and(|s| s.icon.is_some());
                    point_layout(
                        c.at,
                        &l.text,
                        c.size,
                        icon,
                        pad,
                        &self.grid,
                        &mut self.pens,
                        &mut self.boxes,
                    )
                }
            };
            if !placed {
                continue;
            }
            let opacity = match c.shown {
                Some(o) => (o + fade).min(1.0),
                None if fade.is_infinite() => 1.0,
                None => 0.0,
            };
            let start = out.glyphs.len();
            let halo = rule.halo_width * ppp;
            if !emit(
                &mut self.atlas,
                &self.pens,
                rule,
                halo,
                opacity,
                srgb,
                &mut out.glyphs,
            ) {
                out.glyphs.truncate(start);
                continue;
            }
            for b in &self.boxes {
                self.grid.insert(*b);
            }
            if c.line.is_some() {
                lines.entry(l.key).or_default().push(c.at);
            }
            shown.entry(l.key).or_default().push(Shown {
                at: c.world,
                opacity,
            });
            out.labels += 1;
            out.fading |= opacity < 1.0;
        }
        self.shown = shown;
        out
    }
}

/// The point at distance `d` along a line with cumulative distances `dists`.
fn point_at(points: &[[f32; 2]], dists: &[f32], d: f32) -> [f32; 2] {
    let i = dists
        .partition_point(|&x| x <= d)
        .clamp(1, points.len() - 1);
    let (a, b) = (points[i - 1], points[i]);
    let span = dists[i] - dists[i - 1];
    let t = if span > 0.0 {
        ((d - dists[i - 1]) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Lays `text` along the line, centred at distance `d`, glyph by glyph. False where the
/// line bends too much.
#[allow(clippy::too_many_arguments)]
fn line_layout(
    points: &[[f32; 2]],
    dists: &[f32],
    d: f32,
    text: &Text,
    size: f32,
    pad: f32,
    pens: &mut Vec<Pen>,
    boxes: &mut Vec<[f32; 4]>,
) -> bool {
    let half = text.width * size / 2.0;
    let a = point_at(points, dists, d - half);
    let b = point_at(points, dists, d + half);
    // Read left to right: along the line or against it.
    let sign = if b[0] < a[0] { -1.0 } else { 1.0 };
    let (mut prev, mut first) = (None, 0.0);
    for g in &text.glyphs {
        let adv = g.advance * size / 2.0;
        let t = d + sign * ((g.x * size + adv) - half);
        let h = adv.max(1.0);
        let p0 = point_at(points, dists, t - sign * h);
        let p1 = point_at(points, dists, t + sign * h);
        let angle = (p1[1] - p0[1]).atan2(p1[0] - p0[0]);
        if let Some(p) = prev {
            if turn(angle, p) > MAX_BEND || turn(angle, first) > MAX_TURN {
                return false;
            }
        } else {
            first = angle;
        }
        prev = Some(angle);
        let dir = [angle.cos(), angle.sin()];
        let n = [-dir[1], dir[0]];
        let c = point_at(points, dists, t);
        pens.push(Pen {
            at: [
                c[0] - dir[0] * adv + n[0] * MID_CAP * size,
                c[1] - dir[1] * adv + n[1] * MID_CAP * size,
            ],
            dir,
            id: g.id,
            font: text.font,
            size,
        });
        let r = adv.max(0.55 * size) + pad;
        boxes.push([c[0] - r, c[1] - r, c[0] + r, c[1] + r]);
    }
    true
}

/// The angle between two directions, 0–π.
fn turn(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::TAU);
    d.min(std::f32::consts::TAU - d)
}

/// Lays out a point label at `at`: centred on it, or with a POI marker on it and the text at
/// the first free position beside it. False where nothing fits.
#[allow(clippy::too_many_arguments)]
fn point_layout(
    at: [f32; 2],
    text: &Text,
    size: f32,
    marker: bool,
    pad: f32,
    grid: &Grid,
    pens: &mut Vec<Pen>,
    boxes: &mut Vec<[f32; 4]>,
) -> bool {
    let (w, h) = (
        text.width * size,
        f32::from(text.lines) * LINE_HEIGHT * size,
    );
    let mut spots = [[0.0f32; 2]; 4];
    let spots: &[[f32; 2]] = if marker {
        let r = 0.3 * MARKER_SIZE * size;
        let (x, y) = (at[0], at[1]);
        let m = [x - r - pad, y - r - pad, x + r + pad, y + r + pad];
        if grid.hits(&m) {
            return false;
        }
        boxes.push(m);
        let gap = r + 0.25 * size;
        spots = [
            [x + gap, y - h / 2.0],
            [x - gap - w, y - h / 2.0],
            [x - w / 2.0, y + gap],
            [x - w / 2.0, y - gap - h],
        ];
        &spots
    } else {
        spots[0] = [at[0] - w / 2.0, at[1] - h / 2.0];
        &spots[..1]
    };
    // Against other labels only: the text's padding may overlap its own marker's.
    let Some(&[left, top]) = spots
        .iter()
        .find(|s| !grid.hits(&[s[0] - pad, s[1] - pad, s[0] + w + pad, s[1] + h + pad]))
    else {
        return false;
    };
    // Whole pixels, so horizontal text is as sharp as it can be.
    let (left, top) = (left.round(), top.round());
    boxes.push([left - pad, top - pad, left + w + pad, top + h + pad]);
    if marker {
        let fonts = text::fonts();
        let m = fonts.shape(Font::Regular, MARKER);
        let ms = MARKER_SIZE * size;
        if let Some(g) = m.glyphs.first() {
            pens.push(Pen {
                at: [at[0] - g.advance * ms / 2.0, at[1] + MID_CAP * ms],
                dir: [1.0, 0.0],
                id: g.id,
                font: Font::Regular,
                size: ms,
            });
        }
    }
    for g in &text.glyphs {
        pens.push(Pen {
            at: [
                left + g.x * size,
                top + (g.y + LINE_HEIGHT / 2.0 + MID_CAP) * size,
            ],
            dir: [1.0, 0.0],
            id: g.id,
            font: text.font,
            size,
        });
    }
    true
}

/// Adds the glyph quads of `pens`, with a halo `halo` pixels wide. False if the atlas is
/// full this frame.
fn emit(
    atlas: &mut Atlas,
    pens: &[Pen],
    rule: &Label,
    halo: f32,
    opacity: f32,
    srgb: bool,
    out: &mut Vec<GlyphInstance>,
) -> bool {
    let color = |c| {
        let [r, g, b, a] = gpu_color(c, srgb);
        [r, g, b, a * opacity].map(|v| (v * 255.0).round() as u8)
    };
    let (fill, halo_color) = (color(rule.color), color(rule.halo));
    let atlas_px = 1.0 / ATLAS as f32;
    for p in pens {
        let slot = match atlas.glyph(p.font, p.id) {
            None => return false,
            Some(None) => continue,
            Some(Some(s)) => s,
        };
        let k = p.size / SDF_EM;
        let (u, n) = (p.dir, [-p.dir[1], p.dir[0]]);
        let (hw, hh) = (f32::from(slot.w) * k / 2.0, f32::from(slot.h) * k / 2.0);
        let (ox, oy) = (slot.left * k + hw, slot.top * k + hh);
        let (x, y) = (f32::from(slot.x), f32::from(slot.y));
        out.push(GlyphInstance {
            center: [
                p.at[0] + u[0] * ox + n[0] * oy,
                p.at[1] + u[1] * ox + n[1] * oy,
            ],
            axis: u,
            half: [hw, hh],
            uv: [
                x * atlas_px,
                y * atlas_px,
                (x + f32::from(slot.w)) * atlas_px,
                (y + f32::from(slot.h)) * atlas_px,
            ],
            color: fill,
            halo: halo_color,
            scale: [k, halo],
        });
    }
    true
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn overlaps(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// Placed labels' boxes, in a grid of screen cells.
#[derive(Default)]
struct Grid {
    cols: i32,
    rows: i32,
    cells: Vec<Vec<u32>>,
    boxes: Vec<[f32; 4]>,
}

const CELL: f32 = 64.0;

impl Grid {
    fn reset(&mut self, view: [f32; 2]) {
        self.cols = (view[0] / CELL).ceil().max(1.0) as i32;
        self.rows = (view[1] / CELL).ceil().max(1.0) as i32;
        let n = (self.cols * self.rows) as usize;
        self.cells.resize_with(n, Vec::new);
        self.cells.truncate(n);
        for c in &mut self.cells {
            c.clear();
        }
        self.boxes.clear();
    }

    /// The cells a box touches, clamped to the grid.
    fn cells(&self, b: &[f32; 4]) -> impl Iterator<Item = usize> + use<> {
        let cell = |v: f32, n: i32| ((v / CELL).floor() as i32).clamp(0, n - 1);
        let (x0, x1) = (cell(b[0], self.cols), cell(b[2], self.cols));
        let (y0, y1) = (cell(b[1], self.rows), cell(b[3], self.rows));
        let cols = self.cols;
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (y * cols + x) as usize))
    }

    fn hits(&self, b: &[f32; 4]) -> bool {
        self.cells(b).any(|c| {
            self.cells[c]
                .iter()
                .any(|&i| overlaps(&self.boxes[i as usize], b))
        })
    }

    fn insert(&mut self, b: [f32; 4]) {
        let i = self.boxes.len() as u32;
        self.boxes.push(b);
        for c in self.cells(&b) {
            self.cells[c].push(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::StyleFile;
    use omp_core::schema::Schema;
    use omp_core::whitelist::{DEFAULT, Whitelist};

    const TILE: TileId = TileId::new(10, 300, 400);

    fn style() -> (Style, Schema) {
        let schema = Schema::from_whitelist(&Whitelist::parse(DEFAULT).unwrap());
        let (style, _) = Style::compile(&StyleFile::builtin(), &schema);
        (style, schema)
    }

    /// A camera at zoom 10 with `TILE` in the middle of a 1024-point square view.
    fn camera() -> Camera {
        let mut cam = Camera::new([0.0, 0.0, 1.0, 1.0]);
        cam.size = [1024.0, 1024.0];
        let [x0, y0, x1, y1] = TILE.world_rect();
        cam.center = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
        cam.zoom = 10.0;
        cam
    }

    fn label(
        schema: &Schema,
        layer: &str,
        class: &str,
        name: &str,
        rank: f32,
        geom: Geom,
    ) -> TileLabel {
        let (l, sl) = schema.layer(layer).unwrap();
        let font = if layer == "waterways" {
            Font::Italic
        } else {
            Font::Regular
        };
        let mut h = std::hash::DefaultHasher::new();
        (l, name).hash(&mut h);
        TileLabel {
            layer: l,
            class: sl.class(class).unwrap(),
            text: Arc::new(text::fonts().shape(font, name)),
            key: h.finish(),
            rank,
            geom,
        }
    }

    fn place(
        p: &mut Placer,
        cam: &Camera,
        style: &Style,
        labels: &TileLabels,
        square: TileId,
    ) -> Placed {
        let src = [Source {
            labels,
            tile: TILE,
            square,
            cap: None,
        }];
        p.place(cam, 1.0, style, &src, false, None)
    }

    /// The labels shown, by key, with their anchors in world coordinates.
    fn shown(p: &Placer) -> Vec<(u64, [i64; 2])> {
        let mut v: Vec<_> = p
            .shown
            .iter()
            .flat_map(|(k, s)| s.iter().map(|s| (*k, s.at.map(|c| (c * 1e9) as i64))))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn labels_do_not_overlap_and_hold_still_while_panning() {
        let (style, schema) = style();
        // A crowd of towns, many of them too close to all fit.
        let mut labels = TileLabels::default();
        for i in 0..60 {
            let p = [(i * 523 % 3600 + 200) as i16, (i * 797 % 3600 + 200) as i16];
            labels.labels.push(label(
                &schema,
                "places",
                "town",
                &format!("Town {i}"),
                0.0,
                Geom::Point(p),
            ));
        }
        let mut p = Placer::default();
        let mut cam = camera();
        let first = place(&mut p, &cam, &style, &labels, TILE);
        assert!(
            first.labels > 10 && first.labels < 60,
            "{} placed",
            first.labels
        );
        let placed = shown(&p);
        for b in 0..p.grid.boxes.len() {
            for a in 0..b {
                assert!(!overlaps(&p.grid.boxes[a], &p.grid.boxes[b]));
            }
        }
        // Small pans keep the same labels at the same places on the map.
        for step in 0..20 {
            cam.pan([3.0, -2.0]);
            let again = place(&mut p, &cam, &style, &labels, TILE);
            assert_eq!(shown(&p), placed, "step {step}");
            // The glyphs moved with the map.
            let d = [
                again.glyphs[0].center[0] - first.glyphs[0].center[0],
                again.glyphs[0].center[1] - first.glyphs[0].center[1],
            ];
            let want = [3.0 * (step + 1) as f32, -2.0 * (step + 1) as f32];
            assert!(
                (d[0] - want[0]).abs() < 0.01 && (d[1] - want[1]).abs() < 0.01,
                "{d:?}"
            );
        }
    }

    #[test]
    fn a_shown_label_keeps_its_place_against_an_equal_one() {
        let (style, schema) = style();
        // Two villages ten units apart: they cannot both be shown. "Big" ranks higher, but
        // starts off screen.
        let mut labels = TileLabels::default();
        labels.labels.push(label(
            &schema,
            "places",
            "village",
            "Big",
            5.0,
            Geom::Point([2048, 2048]),
        ));
        labels.labels.push(label(
            &schema,
            "places",
            "village",
            "Small",
            0.0,
            Geom::Point([2058, 2048]),
        ));
        let mut p = Placer::default();
        let mut cam = camera();
        // Big is 512 points right of the left edge, Small 1.25 further.
        cam.pan([-512.5, 0.0]);
        place(&mut p, &cam, &style, &labels, TILE);
        let small = labels.labels[1].key;
        assert_eq!(
            shown(&p).iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![small]
        );
        // Both on screen now: Small keeps its place.
        cam.pan([2.0, 0.0]);
        place(&mut p, &cam, &style, &labels, TILE);
        assert_eq!(
            shown(&p).iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![small]
        );
        // Without the last frame, Big wins.
        let mut fresh = Placer::default();
        place(&mut fresh, &cam, &style, &labels, TILE);
        assert_eq!(
            shown(&fresh).iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![labels.labels[0].key]
        );
    }

    #[test]
    fn a_poi_has_a_marker_and_its_name_beside_it() {
        let (style, schema) = style();
        let mut cam = camera();
        cam.zoom = 14.0;
        let mut labels = TileLabels::default();
        labels.labels.push(label(
            &schema,
            "pois",
            "attraction",
            "Empire State Building",
            0.0,
            Geom::Point([2048, 2048]),
        ));
        // A town label right of it takes the first spot, so the name goes left.
        let mut town = label(
            &schema,
            "places",
            "town",
            "Townsville",
            0.0,
            Geom::Point([2048 + 40, 2048]),
        );
        town.rank = 1.0;
        labels.labels.push(town);
        let mut p = Placer::default();
        let placed = place(&mut p, &cam, &style, &labels, TILE);
        assert_eq!(placed.labels, 2);
        let n = "Empire State Building"
            .chars()
            .filter(|c| *c != ' ')
            .count();
        // The town's glyphs come first (higher priority), then the marker and the name.
        let poi = &placed.glyphs[placed.glyphs.len() - n - 1..];
        let (marker, name) = (poi[0], &poi[1..]);
        assert!(
            name.iter().all(|g| g.center[0] < marker.center[0]),
            "the name is left of the marker"
        );
        assert!(
            name.iter()
                .all(|g| (g.center[1] - marker.center[1]).abs() < 10.0)
        );
    }

    #[test]
    fn line_labels_read_left_to_right_and_skip_sharp_bends() {
        let (style, schema) = style();
        let mut cam = camera();
        // Rivers are labelled from z11.
        cam.zoom = 11.0;
        // Drawn right to left; the text still runs left to right.
        let west = Geom::Line(vec![[3800, 2000], [200, 2000]].into());
        let mut labels = TileLabels::default();
        labels.labels.push(label(
            &schema,
            "waterways",
            "river",
            "Long River",
            0.0,
            west,
        ));
        let mut p = Placer::default();
        let placed = place(&mut p, &cam, &style, &labels, TILE);
        // One anchor fits per REPEAT: 4096 units at z11 are 1024 points.
        assert!(placed.labels >= 2, "{}", placed.labels);
        assert!(placed.glyphs.iter().all(|g| g.axis[0] > 0.99));
        let xs: Vec<f32> = placed.glyphs.iter().take(4).map(|g| g.center[0]).collect();
        assert!(xs.windows(2).all(|w| w[1] > w[0]), "{xs:?}");
        // A zigzag at right angles every few points is not labelled.
        let zigzag: Vec<[i16; 2]> = (0..80)
            .map(|i| [200 + i * 40, 2000 + (i % 2) * 40])
            .collect();
        let mut labels = TileLabels::default();
        labels.labels.push(label(
            &schema,
            "waterways",
            "river",
            "Zigzag",
            0.0,
            Geom::Line(zigzag.into()),
        ));
        assert_eq!(
            place(&mut Placer::default(), &cam, &style, &labels, TILE).labels,
            0
        );
    }

    #[test]
    fn a_stand_in_tile_labels_only_its_square_and_new_labels_fade_in() {
        let (style, schema) = style();
        let mut labels = TileLabels::default();
        // One town in each quarter of the tile.
        for (i, p) in [[1000, 1000], [3000, 1000], [1000, 3000], [3000, 3000]]
            .into_iter()
            .enumerate()
        {
            labels.labels.push(label(
                &schema,
                "places",
                "town",
                &format!("T{i}"),
                0.0,
                Geom::Point(p),
            ));
        }
        let cam = camera();
        let child = TILE.children()[1];
        let mut p = Placer::default();
        assert_eq!(place(&mut p, &cam, &style, &labels, child).labels, 1);
        assert_eq!(shown(&p)[0].0, labels.labels[1].key);
        // With a clock, new labels start transparent and fade in.
        let src = [Source {
            labels: &labels,
            tile: TILE,
            square: TILE,
            cap: None,
        }];
        let mut p = Placer::default();
        let t0 = Instant::now();
        let a = p.place(&cam, 1.0, &style, &src, false, Some(t0));
        assert!(a.fading && a.glyphs.iter().all(|g| g.color[3] == 0));
        let b = p.place(
            &cam,
            1.0,
            &style,
            &src,
            false,
            Some(t0 + std::time::Duration::from_millis(100)),
        );
        assert!(
            b.fading
                && b.glyphs
                    .iter()
                    .all(|g| g.color[3] > 100 && g.color[3] < 155)
        );
        let c = p.place(
            &cam,
            1.0,
            &style,
            &src,
            false,
            Some(t0 + std::time::Duration::from_millis(300)),
        );
        assert!(!c.fading && c.glyphs.iter().all(|g| g.color[3] == 255));
    }
}
