//! Text for map labels (phase1-plan.md 1.7): the bundled Noto Sans faces, shaping, and the
//! signed distance field glyphs the labels are drawn with.
//!
//! Shaping uses `harfrust` and outlines come from `skrifa`, both already in the tree through
//! egui (decision 9), so kerning comes from the font's `GPOS` at no cost in binary size.
//! Text is laid out in ems, x to the right and y down from the first baseline, so one
//! shaping serves every text size.
//!
//! Each glyph's distance field is made once from its outline, at [`SDF_EM`] atlas pixels per
//! em, and scaled on the GPU; the halo is a second, wider edge of the same field. Loader
//! threads make the fields of the glyphs their tiles need ([`glyph_sdf`]); the UI thread packs
//! them into the [`Atlas`] when a label first uses them. Shields and icons (shapes.rs) are
//! distance fields in the same atlas.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

use crate::shapes::{self, ShapeKey};
use crate::style::Font;

/// Subset to Latin by `data/fonts/subset.sh`, in [`Font`] order.
const DATA: [&[u8]; 3] = [
    include_bytes!("../data/fonts/NotoSans-Regular.ttf"),
    include_bytes!("../data/fonts/NotoSans-Italic.ttf"),
    include_bytes!("../data/fonts/NotoSans-Bold.ttf"),
];

/// Line spacing of wrapped labels, in ems.
pub const LINE_HEIGHT: f32 = 1.2;
/// From the baseline up to the middle of a capital letter, in ems: text centred on a point
/// or a line is placed this far below it.
pub const MID_CAP: f32 = 0.357;

/// Atlas pixels per em of the distance fields.
pub const SDF_EM: f32 = 32.0;
/// A field byte `v` is `255 · (EDGE − d / SDF_RANGE)`, clamped, for a distance `d` in atlas
/// pixels outside the outline (negative inside). So it reaches `EDGE · SDF_RANGE` = 6 pixels
/// out, which bounds the halo, and 2 pixels in.
pub const SDF_RANGE: f32 = 8.0;
pub const SDF_EDGE: f32 = 0.75;
/// Blank pixels around each outline: as far as the field reaches.
const PAD: f32 = 6.0;

struct Face {
    font: skrifa::FontRef<'static>,
    shaper: harfrust::ShaperData,
    /// Made once: planning is most of the cost of shaping a short name.
    plan: harfrust::ShapePlan,
    upem: f32,
}

pub struct Fonts {
    faces: [Face; 3],
}

/// One shaped glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: u16,
    /// Pen position, in ems.
    pub x: f32,
    pub y: f32,
    pub advance: f32,
}

/// Shaped text.
#[derive(Debug, PartialEq)]
pub struct Text {
    pub font: Font,
    pub glyphs: Vec<Glyph>,
    /// The widest line, in ems.
    pub width: f32,
    pub lines: u8,
}

impl Text {
    /// Heap and inline bytes, for the tile cache.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(self.glyphs.as_slice())
    }
}

/// The bundled fonts, loaded on first use.
pub fn fonts() -> &'static Fonts {
    static FONTS_: OnceLock<Fonts> = OnceLock::new();
    FONTS_.get_or_init(|| Fonts {
        faces: DATA.map(|data| {
            let font = skrifa::FontRef::new(data).expect("a bundled font parses");
            let upem = font
                .metrics(Size::unscaled(), LocationRef::default())
                .units_per_em;
            let shaper = harfrust::ShaperData::new(&font);
            let plan = harfrust::ShapePlan::new(
                &shaper.shaper(&font).build(),
                harfrust::Direction::LeftToRight,
                Some(harfrust::script::LATIN),
                None,
                &[],
            );
            Face {
                shaper,
                plan,
                font,
                upem: f32::from(upem),
            }
        }),
    })
}

impl Fonts {
    fn face(&self, font: Font) -> &Face {
        &self.faces[font as usize]
    }

    /// Whether the font has a glyph for every character of `text`; labels in other scripts
    /// are left out (Latin only, phase 1).
    pub fn covers(&self, font: Font, text: &str) -> bool {
        let cmap = self.face(font).font.charmap();
        text.chars()
            .all(|c| c.is_whitespace() || cmap.map(c).is_some())
    }

    /// Shapes `text` on one line.
    pub fn shape(&self, font: Font, text: &str) -> Text {
        let mut glyphs = Vec::with_capacity(text.len());
        let width = self.shape_run(font, text, 0.0, 0.0, &mut glyphs);
        Text {
            font,
            glyphs,
            width,
            lines: 1,
        }
    }

    /// Shapes `text`, broken at spaces into lines of at most `max` ems where it can be, each
    /// line centred on the widest.
    pub fn shape_wrapped(&self, font: Font, text: &str, max: f32) -> Text {
        let one = self.shape(font, text);
        if one.width <= max || !text.contains(' ') {
            return one;
        }
        let space = self.shape(font, " ").width;
        let words: Vec<(&str, f32)> = text
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(|w| (w, self.shape(font, w).width))
            .collect();
        // Greedy: a word goes on the current line while the line stays within `max`.
        let mut lines: Vec<(String, f32)> = Vec::new();
        for (w, wd) in words {
            match lines.last_mut() {
                Some((l, lw)) if *lw + space + wd <= max => {
                    l.push(' ');
                    l.push_str(w);
                    *lw += space + wd;
                }
                _ => lines.push((w.to_string(), wd)),
            }
        }
        let width = lines.iter().map(|l| l.1).fold(0.0, f32::max);
        let mut glyphs = Vec::with_capacity(text.len());
        for (i, (l, lw)) in lines.iter().enumerate() {
            let y = i as f32 * LINE_HEIGHT;
            self.shape_run(font, l, (width - lw) / 2.0, y, &mut glyphs);
        }
        Text {
            font,
            glyphs,
            width,
            lines: lines.len().min(255) as u8,
        }
    }

    /// Shapes one line starting at `(x, y)` ems; returns its advance.
    fn shape_run(&self, font: Font, text: &str, x: f32, y: f32, out: &mut Vec<Glyph>) -> f32 {
        let face = self.face(font);
        let shaper = face.shaper.shaper(&face.font).build();
        let mut buf = harfrust::UnicodeBuffer::new();
        buf.push_str(text);
        // As planned: Latin only (phase 1), so no guessing.
        buf.set_direction(harfrust::Direction::LeftToRight);
        buf.set_script(harfrust::script::LATIN);
        let shaped = shaper.shape(buf, harfrust::ShapeOptions::new().plan(Some(&face.plan)));
        let k = 1.0 / face.upem;
        let mut pen = 0.0;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let advance = pos.x_advance as f32 * k;
            out.push(Glyph {
                id: info.glyph_id as u16,
                x: x + pen + pos.x_offset as f32 * k,
                y: y - pos.y_offset as f32 * k,
                advance,
            });
            pen += advance;
        }
        pen
    }
}

/// A glyph's (or shape's) distance field.
#[derive(Debug)]
pub struct GlyphSdf {
    pub w: u16,
    pub h: u16,
    /// The bitmap's top-left corner from the pen position, in atlas pixels, y down.
    pub left: f32,
    pub top: f32,
    /// Atlas pixels per em: [`SDF_EM`] for glyphs, less for shields.
    pub em: f32,
    pub data: Vec<u8>,
}

/// What the atlas holds: glyphs, and the layers of shields and icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sprite {
    Glyph(Font, u16),
    Shape(ShapeKey),
}

type GlyphKey = (Font, u16);

/// The distance field of a glyph, made on first use and kept; `None` for a glyph without
/// ink, such as a space.
pub fn glyph_sdf(font: Font, id: u16) -> Option<Arc<GlyphSdf>> {
    static CACHE: OnceLock<Mutex<HashMap<GlyphKey, Option<Arc<GlyphSdf>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(g) = cache.lock().unwrap().get(&(font, id)) {
        return g.clone();
    }
    // Made outside the lock; two threads may both make it, and one is kept.
    let g = make_sdf(fonts().face(font), id).map(Arc::new);
    cache.lock().unwrap().entry((font, id)).or_insert(g).clone()
}

/// A line segment of an outline, in atlas pixels with y down.
pub type Segment = [[f32; 2]; 2];

/// Flattens outlines into line segments, in atlas pixels with y down: a point `(x, y)` goes
/// to `offset + scale · (x, y)`.
pub struct Flatten {
    scale: [f32; 2],
    offset: [f32; 2],
    pub segs: Vec<Segment>,
    start: [f32; 2],
    at: [f32; 2],
}

impl Flatten {
    pub fn new(scale: [f32; 2], offset: [f32; 2]) -> Self {
        Self {
            scale,
            offset,
            segs: Vec::new(),
            start: [0.0; 2],
            at: [0.0; 2],
        }
    }

    /// Changes the transform for the outlines drawn next.
    pub fn set_transform(&mut self, scale: [f32; 2], offset: [f32; 2]) {
        self.scale = scale;
        self.offset = offset;
    }

    fn p(&self, x: f32, y: f32) -> [f32; 2] {
        [
            self.offset[0] + x * self.scale[0],
            self.offset[1] + y * self.scale[1],
        ]
    }

    fn to(&mut self, p: [f32; 2]) {
        if p != self.at {
            self.segs.push([self.at, p]);
        }
        self.at = p;
    }

    /// Pieces for a curve with control polygon length `len`.
    fn pieces(len: f32) -> usize {
        (len / 3.0).ceil().clamp(2.0, 16.0) as usize
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (b[0] - a[0]).hypot(b[1] - a[1])
}

impl OutlinePen for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = self.p(x, y);
        self.at = self.start;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.to(p);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (a, c, b) = (self.at, self.p(cx, cy), self.p(x, y));
        let n = Self::pieces(dist(a, c) + dist(c, b));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let q = |k: usize| u * u * a[k] + 2.0 * u * t * c[k] + t * t * b[k];
            self.to([q(0), q(1)]);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, c0, c1, b) = (self.at, self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
        let n = Self::pieces(dist(a, c0) + dist(c0, c1) + dist(c1, b));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let q = |k: usize| {
                u * u * u * a[k]
                    + 3.0 * u * u * t * c0[k]
                    + 3.0 * u * t * t * c1[k]
                    + t * t * t * b[k]
            };
            self.to([q(0), q(1)]);
        }
    }

    fn close(&mut self) {
        let s = self.start;
        self.to(s);
    }
}

fn make_sdf(face: &Face, id: u16) -> Option<GlyphSdf> {
    let outline = face
        .font
        .outline_glyphs()
        .get(GlyphId::new(u32::from(id)))?;
    let k = SDF_EM / face.upem;
    let mut pen = Flatten::new([k, -k], [0.0; 2]);
    outline
        .draw(
            DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
            &mut pen,
        )
        .ok()?;
    pen.close();
    distance_field(&pen.segs, SDF_EM, 0.0, None)
}

/// The distance field of the outline `segs` (nonzero winding) drawn at `em` atlas pixels per
/// em, with its edge moved `inset` atlas pixels inwards, and only the part above `y < clip`
/// if a clip is given. `None` for an empty outline.
pub fn distance_field(
    segs: &[Segment],
    em: f32,
    inset: f32,
    clip: Option<f32>,
) -> Option<GlyphSdf> {
    if segs.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in segs.iter().flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if let Some(c) = clip {
        hi[1] = hi[1].min(c);
    }
    let x0 = (lo[0] - PAD).floor();
    let y0 = (lo[1] - PAD).floor();
    let w = ((hi[0] + PAD).ceil() - x0) as usize;
    let h = ((hi[1] + PAD).ceil() - y0) as usize;
    let mut data = vec![0u8; w * h];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for j in 0..h {
        let py = y0 + j as f32 + 0.5;
        // Nonzero winding along the row: where each edge crosses it, and which way.
        crossings.clear();
        for &[a, b] in segs {
            if (a[1] <= py) != (b[1] <= py) {
                let t = (py - a[1]) / (b[1] - a[1]);
                crossings.push((a[0] + t * (b[0] - a[0]), if b[1] > a[1] { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut winding, mut next) = (0, 0);
        for i in 0..w {
            let px = x0 + i as f32 + 0.5;
            while next < crossings.len() && crossings[next].0 < px {
                winding += crossings[next].1;
                next += 1;
            }
            let mut d2 = f32::MAX;
            for &[a, b] in segs {
                let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
                let (qx, qy) = (px - a[0], py - a[1]);
                let t = ((qx * ex + qy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
                let (dx, dy) = (qx - t * ex, qy - t * ey);
                d2 = d2.min(dx * dx + dy * dy);
            }
            let mut d = if winding != 0 { -d2.sqrt() } else { d2.sqrt() };
            d += inset;
            if let Some(c) = clip {
                d = d.max(py - c);
            }
            let v = (SDF_EDGE - d / SDF_RANGE).clamp(0.0, 1.0);
            data[j * w + i] = (v * 255.0).round() as u8;
        }
    }
    Some(GlyphSdf {
        w: w as u16,
        h: h as u16,
        left: x0,
        top: y0,
        em,
        data,
    })
}

/// Atlas texture size, in pixels; a single channel.
pub const ATLAS: u32 = 1024;

/// Where a glyph is in the atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// As in [`GlyphSdf`].
    pub left: f32,
    pub top: f32,
    pub em: f32,
}

/// Glyph bitmaps to copy into the atlas texture.
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub sdf: Arc<GlyphSdf>,
}

/// The glyphs on the GPU: shelves of glyph fields, packed as labels first use them. When it
/// is full it starts over on the next frame, and the glyphs that frame needs are added again.
pub struct Atlas {
    /// Width and height in pixels.
    size: u32,
    slots: HashMap<Sprite, Option<Slot>>,
    /// Shelves: top, height, and the x where the next glyph goes.
    shelves: Vec<[u32; 3]>,
    full: bool,
    uploads: Vec<Upload>,
}

/// Pixels between glyphs, so filtering never mixes in a neighbour.
const GUTTER: u32 = 1;

impl Default for Atlas {
    fn default() -> Self {
        Self::new(ATLAS)
    }
}

impl Atlas {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            slots: HashMap::new(),
            shelves: Vec::new(),
            full: false,
            uploads: Vec::new(),
        }
    }

    /// Starts over if the atlas filled up in the last frame.
    pub fn begin_frame(&mut self) {
        if self.full {
            self.slots.clear();
            self.shelves.clear();
            self.full = false;
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// The share of the atlas's height its shelves take.
    pub fn fill(&self) -> f32 {
        let top = self.shelves.last().map_or(0, |s| s[0] + s[1]);
        top as f32 / self.size as f32
    }

    #[cfg(test)]
    pub fn glyph(&mut self, font: Font, id: u16) -> Option<Option<Slot>> {
        self.get(Sprite::Glyph(font, id))
    }

    /// The sprite's slot, packing it on first use. `Some(None)`: a sprite without ink. `None`:
    /// the atlas is full until the next frame.
    pub fn get(&mut self, sprite: Sprite) -> Option<Option<Slot>> {
        if let Some(s) = self.slots.get(&sprite) {
            return Some(*s);
        }
        let sdf = match sprite {
            Sprite::Glyph(font, id) => glyph_sdf(font, id),
            Sprite::Shape(key) => shapes::sdf(key),
        };
        let slot = match sdf {
            None => None,
            Some(sdf) => Some(self.pack(sdf)?),
        };
        self.slots.insert(sprite, slot);
        Some(slot)
    }

    fn pack(&mut self, sdf: Arc<GlyphSdf>) -> Option<Slot> {
        let (w, h) = (u32::from(sdf.w) + GUTTER, u32::from(sdf.h) + GUTTER);
        // The first shelf it fits without wasting much height, else a new one.
        let fit = self
            .shelves
            .iter()
            .position(|s| h <= s[1] && s[1] <= h + 4 && s[2] + w <= self.size);
        let i = match fit {
            Some(i) => i,
            None => {
                let top = self.shelves.last().map_or(0, |s| s[0] + s[1]);
                if top + h > self.size || w > self.size {
                    self.full = true;
                    return None;
                }
                self.shelves.push([top, h, 0]);
                self.shelves.len() - 1
            }
        };
        let s = &mut self.shelves[i];
        let slot = Slot {
            x: s[2] as u16,
            y: s[0] as u16,
            w: sdf.w,
            h: sdf.h,
            left: sdf.left,
            top: sdf.top,
            em: sdf.em,
        };
        s[2] += w;
        self.uploads.push(Upload {
            x: u32::from(slot.x),
            y: u32::from(slot.y),
            sdf,
        });
        Some(slot)
    }

    /// The glyphs packed since the last call, for the GPU.
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.uploads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_with_kerning_and_wraps() {
        let f = fonts();
        let av = f.shape(Font::Regular, "AV");
        let a = f.shape(Font::Regular, "A").width;
        let v = f.shape(Font::Regular, "V").width;
        assert_eq!(av.glyphs.len(), 2);
        assert!(
            av.width < a + v - 0.01,
            "AV is kerned: {} vs {}",
            av.width,
            a + v
        );
        assert!(f.covers(Font::Italic, "Saint-Jean-sur-Richelieu, Île d'Orléans"));
        assert!(!f.covers(Font::Regular, "ᐃᖃᓗᐃᑦ"));
        let t = f.shape_wrapped(Font::Bold, "Saint-Jean-sur-Richelieu Est Nord", 8.0);
        assert!(
            t.lines >= 2 && t.width <= f.shape(Font::Bold, "Saint-Jean-sur-Richelieu").width + 1e-4
        );
        let last = t.glyphs.last().unwrap();
        assert_eq!(last.y, f32::from(t.lines - 1) * LINE_HEIGHT);
        // No break without a space.
        assert_eq!(f.shape_wrapped(Font::Bold, "Mississauga", 2.0).lines, 1);
    }

    #[test]
    fn distance_field_is_inside_on_ink() {
        let t = fonts().shape(Font::Bold, "l");
        let g = glyph_sdf(Font::Bold, t.glyphs[0].id).unwrap();
        let (w, h) = (usize::from(g.w), usize::from(g.h));
        // An "l" is a vertical bar: ink in the middle, nothing in the corners.
        let mid = g.data[h / 2 * w + w / 2];
        assert!(mid > (SDF_EDGE * 255.0) as u8, "{mid}");
        assert_eq!(g.data[0], 0);
        assert!(g.top < 0.0 && g.top > -SDF_EM);
        assert!(glyph_sdf(Font::Bold, fonts().shape(Font::Bold, " ").glyphs[0].id).is_none());
    }

    #[test]
    fn atlas_packs_and_starts_over_when_full() {
        let mut a = Atlas::new(256);
        let ids: Vec<u16> = fonts()
            .shape(Font::Regular, "abcdefghijklmnopqrstuvwxyz")
            .glyphs
            .iter()
            .map(|g| g.id)
            .collect();
        let s0 = a.glyph(Font::Regular, ids[0]).unwrap().unwrap();
        let s1 = a.glyph(Font::Regular, ids[1]).unwrap().unwrap();
        let apart = |a: Slot, b: Slot| a.x + a.w <= b.x || a.y + a.h <= b.y;
        assert!(apart(s0, s1), "{s0:?} {s1:?}");
        assert_eq!(a.take_uploads().len(), 2);
        assert_eq!(a.glyph(Font::Regular, ids[0]), Some(Some(s0)));
        assert!(a.take_uploads().is_empty());
        // Fill it: some glyph does not fit, then the next frame starts over.
        let mut n = 0;
        while a.glyph(Font::Bold, n).is_some() {
            n += 1;
        }
        assert!(n > 20, "{n} glyphs fit");
        a.begin_frame();
        assert_eq!(a.len(), 0);
        assert!(a.glyph(Font::Regular, ids[0]).unwrap().is_some());
    }
}
