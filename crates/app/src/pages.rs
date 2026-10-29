//! Drawing the printed pages (phase 4 step 4.9), as raster images at the printer's
//! resolution (decision 10): the same pages go to the preview, the PDF and, in step 4.10,
//! the printer.
//!
//! A map page is the map renderer's output for the area chosen ([`crate::print::Area`]),
//! with the label, scale bar, stop pins, pushpin names and credit line drawn over it by egui
//! on a transparent layer of the same size. Everything on a page is laid out in the
//! *window's* points, the paper's divided by the text size, and drawn at `dpi / 72 * text
//! size` pixels per point, so the map's labels and egui's text and symbols are sized alike.
//! The directions are the route page's own rows, paginated.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use eframe::egui_wgpu::{self, wgpu};
use omp_core::geo;

use crate::layers::{Hidden, Layers};
use crate::packs::Opened;
use crate::pins::{self, Pin};
use crate::print::{self, Area, CREDIT, Layout, TextSize};
use crate::render;
use crate::routing::{DirectionsDoc, Line, Look, PrintRows};
use crate::scale::{self, Units};
use crate::search;
use crate::snapshot::{self, Gpu, View};
use crate::style::Style;
use crate::theme::{self, INK, WEAK};

/// The biggest side of a page image, the adapter's texture limit (render.rs).
const MAX_SIDE: f64 = 8192.0;

/// A page: opaque RGB.
pub struct Page {
    pub size: [u32; 2],
    /// On paper, in points.
    pub size_pt: [f64; 2],
    pub rgb: Vec<u8>,
}

/// A stop's pin on the map.
#[derive(Clone, Debug)]
pub struct StopMark {
    pub p: geo::Unit,
    pub fill: Color32,
    pub label: String,
}

/// What a map page holds.
#[derive(Clone)]
pub struct MapPage {
    pub layout: Layout,
    pub area: Area,
    pub label: String,
    pub units: Units,
    pub layers: Layers,
    pub dpi: f64,
    pub msaa: u32,
    pub lines: Vec<(Look, Line)>,
    pub stops: Vec<StopMark>,
    pub pins: Vec<Pin>,
}

/// What drawing a map page took, for the log and the checks.
pub struct MapStats {
    pub z: f64,
    pub tiles: usize,
    pub labels: usize,
    pub map_ms: f64,
    pub egui_ms: f64,
}

/// The resolution used for a page: `dpi`, less if the page would not fit a texture.
pub fn page_dpi(layout: &Layout, dpi: f64) -> f64 {
    let side = layout.page[0].max(layout.page[1]);
    dpi.min(MAX_SIDE * 72.0 / side)
}

/// egui drawn offscreen: one context and renderer, for as many pages as are wanted.
pub struct Egui {
    ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
}

impl Egui {
    pub fn new(gpu: &Gpu) -> Self {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        ctx.all_styles_mut(|s| s.animation_time = 0.0);
        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            render::OFFSCREEN_FORMAT,
            egui_wgpu::RendererOptions::PREDICTABLE,
        );
        Self { ctx, renderer }
    }

    /// Runs `build` for a screen of `size` points at `ppp` pixels per point, and draws it on
    /// transparent black: RGBA with premultiplied alpha, row by row.
    pub fn frame(
        &mut self,
        gpu: &Gpu,
        size: Vec2,
        ppp: f32,
        mut build: impl FnMut(&mut egui::Ui),
    ) -> Result<Vec<u8>, String> {
        let px = [(size.x * ppp).round() as u32, (size.y * ppp).round() as u32];
        self.ctx.set_pixels_per_point(ppp);
        let mut deltas = Vec::new();
        let mut shapes = None;
        // Twice: the first pass lays text out with fonts that were only just made.
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                ..Default::default()
            };
            let mut out = self.ctx.run_ui(raw, &mut build);
            deltas.push(std::mem::take(&mut out.textures_delta));
            shapes = Some(out.shapes);
        }
        for mut d in deltas {
            for (id, deltas) in &d.set {
                for delta in deltas {
                    self.renderer
                        .update_texture(&gpu.device, &gpu.queue, *id, delta);
                }
            }
            for id in &d.free {
                self.renderer.free_texture(id);
            }
            d.clear();
        }
        let jobs = self.ctx.tessellate(shapes.unwrap_or_default(), ppp);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: px,
            pixels_per_point: ppp,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui page"),
            size: wgpu::Extent3d {
                width: px[0],
                height: px[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: render::OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let extra = self
            .renderer
            .update_buffers(&gpu.device, &gpu.queue, &mut enc, &jobs, &screen);
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui page"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &jobs, &screen);
        }
        let pixels = render::read_texture(gpu_parts(gpu), enc, &texture, px, extra)?;
        Ok(pixels)
    }
}

fn gpu_parts(gpu: &Gpu) -> (&wgpu::Device, &wgpu::Queue) {
    (&gpu.device, &gpu.queue)
}

/// `top` (premultiplied RGBA) laid over `base` (opaque RGBA), in place.
fn over(base: &mut [u8], top: &[u8]) {
    for (b, t) in base
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(top.as_chunks::<4>().0)
    {
        let keep = 255 - u32::from(t[3]);
        for k in 0..3 {
            b[k] = (u32::from(t[k]) + (u32::from(b[k]) * keep + 127) / 255).min(255) as u8;
        }
    }
}

fn rgb_of(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

/// A white page of `size` pixels.
fn blank(size: [u32; 2]) -> Vec<u8> {
    vec![255; size[0] as usize * size[1] as usize * 4]
}

/// Copies `src` (`w` pixels wide, RGBA) into `dst` (`dw` wide) with its corner at `at`.
fn blit(dst: &mut [u8], dw: usize, src: &[u8], w: usize, at: [usize; 2]) {
    for (y, row) in src.chunks_exact(w * 4).enumerate() {
        let o = ((at[1] + y) * dw + at[0]) * 4;
        dst[o..o + row.len()].copy_from_slice(row);
    }
}

/// Draws one map page: the map, then egui's layer over it.
pub fn render_map(
    gpu: &Gpu,
    opened: &Opened,
    style: &Style,
    egui_pass: &mut Egui,
    page: &MapPage,
) -> Result<(Page, MapStats), String> {
    let t0 = std::time::Instant::now();
    let layout = &page.layout;
    let dpi = page_dpi(layout, page.dpi);
    let s = layout.text.factor();
    let size_px = layout.pixels(dpi);
    // The map's pixels: the map area's, rounded; the camera is sized to them so the view is
    // exactly the area.
    let ppp = dpi / 72.0 * s;
    let map_px = [
        ((layout.map[2] - layout.map[0]) * dpi / 72.0).round() as u32,
        ((layout.map[3] - layout.map[1]) * dpi / 72.0).round() as u32,
    ];
    let mut cam = page.area.camera(layout, opened.bounds);
    cam.size = map_px.map(|v| f64::from(v) / ppp);
    let hidden = Hidden::new(&page.layers, style, &opened.schema);
    let view = View {
        cam: &cam,
        pixels_per_point: ppp as f32,
        msaa: page.msaa,
        hidden: &hidden,
        lines: &page.lines,
        pins: &page.pins,
    };
    let drawn = snapshot::draw_view(gpu, opened, style, &view)?;
    let map_ms = t0.elapsed().as_secs_f64() * 1e3;

    let mut rgba = blank(size_px);
    let at = [
        (layout.map[0] * dpi / 72.0).round() as usize,
        (layout.map[1] * dpi / 72.0).round() as usize,
    ];
    blit(
        &mut rgba,
        size_px[0] as usize,
        &drawn.pixels,
        map_px[0] as usize,
        at,
    );
    drop(drawn.pixels);

    let t1 = std::time::Instant::now();
    // egui's units: the paper's points over the text size.
    let screen = Vec2::new(layout.page[0] as f32, layout.page[1] as f32) / s as f32;
    let origin = Pos2::new((layout.map[0] / s) as f32, (layout.map[1] / s) as f32);
    let top = egui_pass.frame(gpu, screen, ppp as f32, |ui| {
        draw_map_overlay(ui, page, &cam, origin, s)
    })?;
    over(&mut rgba, &top);
    let egui_ms = t1.elapsed().as_secs_f64() * 1e3;
    Ok((
        Page {
            size: size_px,
            size_pt: layout.page,
            rgb: rgb_of(&rgba),
        },
        MapStats {
            z: cam.zoom,
            tiles: drawn.tiles,
            labels: drawn.labels,
            map_ms,
            egui_ms,
        },
    ))
}

/// The label, border, stop pins, pushpin names, scale bar and credit line. Positions are in
/// egui's units; `origin` is the map area's corner.
fn draw_map_overlay(
    ui: &mut egui::Ui,
    page: &MapPage,
    cam: &crate::camera::Camera,
    origin: Pos2,
    s: f64,
) {
    let layout = &page.layout;
    let to_u = |r: [f64; 4]| {
        Rect::from_min_max(
            Pos2::new((r[0] / s) as f32, (r[1] / s) as f32),
            Pos2::new((r[2] / s) as f32, (r[3] / s) as f32),
        )
    };
    let map = to_u(layout.map);
    let p = ui.painter().clone();
    if let Some(h) = layout.header {
        let h = to_u(h);
        p.text(
            Pos2::new(h.left(), h.center().y),
            Align2::LEFT_CENTER,
            &page.label,
            FontId::proportional(15.0),
            INK,
        );
    }
    let credit = to_u(layout.footer);
    p.text(
        Pos2::new(credit.right(), credit.center().y),
        Align2::RIGHT_CENTER,
        CREDIT,
        FontId::proportional(9.0),
        WEAK,
    );
    let on_map = p.with_clip_rect(map);
    pins::draw_names(&on_map, origin, cam, page.pins.iter());
    for stop in page.stops.iter().rev() {
        let w = cam.world_to_screen(stop.p);
        let tip = origin + Vec2::new(w[0] as f32, w[1] as f32);
        search::draw_pin(&on_map, tip, stop.fill, Some(&stop.label));
    }
    // The scale bar, in the map's bottom-left corner on a white card.
    let per_u = page.area.metres_per_page_point(layout) * s;
    scale_bar(
        &on_map,
        map.left_bottom() + Vec2::new(8.0, -8.0),
        per_u,
        page.units,
    );
    p.rect_stroke(map, 0.0, Stroke::new(0.75, INK), egui::StrokeKind::Outside);
}

/// The scale bar with its corner at `corner` (its bottom-left), as the screen's but on a
/// white card so it reads over the map.
fn scale_bar(p: &egui::Painter, corner: Pos2, metres_per_point: f64, units: Units) {
    const MAX: f64 = 120.0;
    const ROW: f32 = 15.0;
    let bars: &[bool] = match units {
        Units::Miles => &[true],
        Units::Kilometres => &[false],
        Units::Both => &[true, false],
    };
    let card = Rect::from_min_size(
        corner - Vec2::new(0.0, ROW * bars.len() as f32 + 6.0),
        Vec2::new(MAX as f32 + 62.0, ROW * bars.len() as f32 + 6.0),
    );
    p.rect_filled(card, 3.0, Color32::from_white_alpha(225));
    let stroke = Stroke::new(1.5, INK);
    for (i, &imperial) in bars.iter().enumerate() {
        let (len, text) = scale::bar(metres_per_point, MAX, imperial);
        let top = card.top() + 3.0 + ROW * i as f32;
        let (x0, x1) = (card.left() + 5.0, card.left() + 5.0 + len as f32);
        let (y, tip) = (top + ROW - 2.0, top + 3.0);
        p.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], stroke);
        p.line_segment([Pos2::new(x0, y), Pos2::new(x0, tip)], stroke);
        p.line_segment([Pos2::new(x1, y), Pos2::new(x1, tip)], stroke);
        p.text(
            Pos2::new(x1 + 5.0, top + ROW / 2.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(11.0),
            INK,
        );
    }
}

/// How the directions' header is built: the label, the totals and the stops, on the
/// first page.
struct Head {
    label: Option<Rect>,
    totals: f32,
    stops: Vec<f32>,
}

const GAP: f32 = 8.0;

/// Draws the directions pages one at a time, giving each to `each` before the next is made.
/// Returns how many there were.
pub fn render_directions(
    gpu: &Gpu,
    egui_pass: &mut Egui,
    doc: &DirectionsDoc,
    paper: print::Paper,
    text: TextSize,
    dpi: f64,
    each: &mut dyn FnMut(Page) -> Result<(), String>,
) -> Result<usize, String> {
    let layout = Layout::new(paper, false, false, text);
    let dpi = page_dpi(&layout, dpi);
    let s = layout.text.factor();
    let ppp = (dpi / 72.0 * s) as f32;
    let size_px = layout.pixels(dpi);
    let screen = Vec2::new(layout.page[0] as f32, layout.page[1] as f32) / s as f32;
    let body = Rect::from_min_max(
        Pos2::new((layout.map[0] / s) as f32, (layout.map[1] / s) as f32),
        Pos2::new((layout.map[2] / s) as f32, (layout.map[3] / s) as f32),
    );
    let footer = Rect::from_min_max(
        Pos2::new((layout.footer[0] / s) as f32, (layout.footer[1] / s) as f32),
        Pos2::new((layout.footer[2] / s) as f32, (layout.footer[3] / s) as f32),
    );
    // A first pass measures the rows and the head, and splits the rows into pages.
    let mut rows = None;
    let mut head = Head {
        label: None,
        totals: 0.0,
        stops: Vec::new(),
    };
    egui_pass.frame(gpu, screen, ppp, |ui| {
        let r = PrintRows::new(ui, doc, body.width());
        head = measure_head(ui, doc, body.width());
        rows = Some(r);
    })?;
    let rows = rows.ok_or("the directions were not laid out")?;
    let head_h = head.label.map_or(0.0, |r| r.height() + 4.0)
        + head.totals
        + head.stops.iter().sum::<f32>()
        + 2.0 * GAP;
    let mut pages: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut start, mut used) = (0, head_h);
    for (i, h) in rows.heights().iter().enumerate() {
        if used + h > body.height() && i > start {
            pages.push(start..i);
            (start, used) = (i, 0.0);
        }
        used += h;
    }
    pages.push(start..rows.heights().len());
    let total = pages.len();
    for (n, range) in pages.into_iter().enumerate() {
        let top = egui_pass.frame(gpu, screen, ppp, |ui| {
            let mut y = body.top();
            if n == 0 {
                y = draw_head(ui, doc, &head, body, y);
            }
            for i in range.clone() {
                let h = rows.heights()[i];
                let rect =
                    Rect::from_min_size(Pos2::new(body.left(), y), Vec2::new(body.width(), h));
                rows.paint(ui, rect, doc, i);
                y += h;
            }
            let p = ui.painter();
            p.text(
                footer.left_center(),
                Align2::LEFT_CENTER,
                format!("Page {} of {total}", n + 1),
                FontId::proportional(9.0),
                WEAK,
            );
            p.text(
                footer.right_center(),
                Align2::RIGHT_CENTER,
                CREDIT,
                FontId::proportional(9.0),
                WEAK,
            );
        })?;
        let mut rgba = blank(size_px);
        over(&mut rgba, &top);
        each(Page {
            size: size_px,
            size_pt: layout.page,
            rgb: rgb_of(&rgba),
        })?;
    }
    Ok(total)
}

fn measure_head(ui: &egui::Ui, doc: &DirectionsDoc, width: f32) -> Head {
    let p = ui.painter();
    let label = (!doc.label.is_empty()).then(|| {
        let g = p.layout(doc.label.clone(), FontId::proportional(17.0), INK, width);
        Rect::from_min_size(Pos2::ZERO, g.size())
    });
    let totals = p
        .layout_no_wrap(doc.totals.clone(), FontId::proportional(14.0), INK)
        .size()
        .y
        + 4.0;
    let stops = doc
        .stops
        .iter()
        .map(|(_, name)| {
            p.layout(name.clone(), FontId::proportional(13.0), INK, width - 26.0)
                .size()
                .y
                .max(18.0)
                + 2.0
        })
        .collect();
    Head {
        label,
        totals,
        stops,
    }
}

/// The first page's head; returns where the rows start.
fn draw_head(ui: &mut egui::Ui, doc: &DirectionsDoc, head: &Head, body: Rect, mut y: f32) -> f32 {
    let p = ui.painter().clone();
    if let Some(l) = head.label {
        let g = p.layout(
            doc.label.clone(),
            FontId::proportional(17.0),
            INK,
            body.width(),
        );
        p.galley(Pos2::new(body.left(), y), g, INK);
        y += l.height() + 4.0;
    }
    p.text(
        Pos2::new(body.left(), y),
        Align2::LEFT_TOP,
        &doc.totals,
        FontId::proportional(14.0),
        INK,
    );
    y += head.totals + GAP;
    for (i, ((fill, name), h)) in doc.stops.iter().zip(&head.stops).enumerate() {
        let c = Pos2::new(body.left() + 9.0, y + 9.0);
        p.circle_filled(c, 8.5, *fill);
        p.text(
            c,
            Align2::CENTER_CENTER,
            (i + 1).to_string(),
            FontId::proportional(10.5),
            Color32::WHITE,
        );
        let g = p.layout(
            name.clone(),
            FontId::proportional(13.0),
            INK,
            body.width() - 26.0,
        );
        p.galley(Pos2::new(body.left() + 24.0, y + 1.0), g, INK);
        y += h;
    }
    y += GAP;
    p.hline(body.x_range(), y - GAP / 2.0, Stroke::new(0.75, WEAK));
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_blends_premultiplied() {
        let mut base = vec![200, 100, 0, 255, 10, 20, 30, 255];
        // Opaque white over the first pixel, half-transparent black (premultiplied: 0s) over
        // the second.
        over(&mut base, &[255, 255, 255, 255, 0, 0, 0, 128]);
        assert_eq!(base[..4], [255, 255, 255, 255]);
        assert_eq!(base[4..7], [5, 10, 15]);
    }

    #[test]
    fn blit_places_the_rows() {
        let mut dst = vec![0u8; 4 * 4 * 4];
        let src = vec![9u8; 2 * 2 * 4];
        blit(&mut dst, 4, &src, 2, [1, 2]);
        let px = |x: usize, y: usize| dst[(y * 4 + x) * 4];
        assert_eq!(px(1, 2), 9);
        assert_eq!(px(2, 3), 9);
        assert_eq!(px(0, 2), 0);
        assert_eq!(px(3, 3), 0);
    }

    #[test]
    fn big_pages_are_given_less_dpi() {
        let l = Layout::new(print::Paper::Tabloid, false, false, TextSize::Normal);
        assert!(page_dpi(&l, 600.0) < 600.0);
        assert!(l.pixels(page_dpi(&l, 600.0))[1] <= 8192);
        let l = Layout::new(print::Paper::Letter, false, false, TextSize::Normal);
        assert_eq!(page_dpi(&l, 300.0), 300.0);
        assert_eq!(page_dpi(&l, 600.0), 600.0);
    }
}
