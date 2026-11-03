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
use crate::theme;

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
        theme::style(&ctx, false);
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
        build: impl FnMut(&mut egui::Ui),
    ) -> Result<Vec<u8>, String> {
        let mut pixels = Vec::new();
        self.frame_rows(gpu, size, ppp, build, &mut |_, row| {
            pixels.extend_from_slice(row)
        })?;
        Ok(pixels)
    }

    /// As [`Self::frame`], handing each row of the layer (premultiplied RGBA) to `rows`.
    pub fn frame_rows(
        &mut self,
        gpu: &Gpu,
        size: Vec2,
        ppp: f32,
        mut build: impl FnMut(&mut egui::Ui),
        rows: &mut dyn FnMut(usize, &[u8]),
    ) -> Result<(), String> {
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
        render::read_texture_rows(gpu_parts(gpu), enc, &texture, px, extra, rows)
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

fn rgb_of(mut rgba: Vec<u8>) -> Vec<u8> {
    // In place: each pixel's three bytes move down over the ones already read.
    let n = rgba.len() / 4;
    for i in 0..n {
        rgba.copy_within(i * 4..i * 4 + 3, i * 3);
    }
    rgba.truncate(n * 3);
    rgba.shrink_to_fit();
    rgba
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
    // Laid over the page row by row as it is read back, so the layer is never held whole.
    let width = size_px[0] as usize;
    egui_pass.frame_rows(
        gpu,
        screen,
        ppp as f32,
        |ui| draw_map_overlay(ui, page, &cam, origin, s),
        &mut |y, row| {
            let at = y * width * 4;
            if let Some(base) = rgba.get_mut(at..at + row.len()) {
                over(base, row);
            }
        },
    )?;
    let egui_ms = t1.elapsed().as_secs_f64() * 1e3;
    Ok((
        Page {
            size: size_px,
            size_pt: layout.page,
            rgb: rgb_of(rgba),
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
            theme::PAPER_INK,
        );
    }
    let credit = to_u(layout.footer);
    p.text(
        Pos2::new(credit.right(), credit.center().y),
        Align2::RIGHT_CENTER,
        CREDIT,
        FontId::proportional(9.0),
        theme::PAPER_WEAK,
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
    let look = scale::Look {
        ink: theme::PAPER_INK,
        card: Some(Color32::from_white_alpha(225)),
    };
    let size = scale::size(&on_map, per_u, page.units, look);
    let at = map.left_bottom() + Vec2::new(8.0, -8.0 - size.y);
    scale::paint(&on_map, at, per_u, page.units, look);
    p.rect_stroke(
        map,
        0.0,
        Stroke::new(0.75, theme::PAPER_INK),
        egui::StrokeKind::Outside,
    );
}

/// How the directions' header is built: the label, the totals, what is said about the
/// route and the stops, on the first page.
struct Head {
    label: Option<Rect>,
    totals: f32,
    notices: Vec<f32>,
    stops: Vec<f32>,
}

const GAP: f32 = 8.0;

/// A piece of the directions, placed on a page whole: the head's lines and rows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Label,
    Totals,
    Notice(usize),
    Stop(usize),
    /// The rule under the head.
    Rule,
    Row(usize),
}

/// The directions measured and split into pages.
struct Plan {
    head: Head,
    rows: PrintRows,
    /// Each page's pieces, with their heights.
    pages: Vec<Vec<(Piece, f32)>>,
}

impl Plan {
    fn new(ui: &egui::Ui, doc: &DirectionsDoc, body: Rect) -> Self {
        let rows = PrintRows::new(ui, doc, body.width());
        let head = measure_head(ui, doc, body.width());
        let pieces = head
            .label
            .map(|r| (Piece::Label, r.height() + 4.0))
            .into_iter()
            .chain([(Piece::Totals, head.totals + GAP)])
            .chain(
                head.notices
                    .iter()
                    .enumerate()
                    .map(|(i, &h)| (Piece::Notice(i), h)),
            )
            .chain(
                head.stops
                    .iter()
                    .enumerate()
                    .map(|(i, &h)| (Piece::Stop(i), h)),
            )
            .chain([(Piece::Rule, GAP)])
            .chain(
                rows.heights()
                    .iter()
                    .enumerate()
                    .map(|(i, &h)| (Piece::Row(i), h)),
            );
        Self {
            pages: paginate(pieces, body.height()),
            head,
            rows,
        }
    }
}

/// Splits `pieces` into pages `room` high: a piece that does not fit what is left of a page
/// starts the next. Only a piece taller than a page, alone on its own, is cut off.
fn paginate(pieces: impl Iterator<Item = (Piece, f32)>, room: f32) -> Vec<Vec<(Piece, f32)>> {
    let mut pages = vec![Vec::new()];
    let mut used = 0.0;
    for (piece, h) in pieces {
        let page = pages.last_mut().expect("there is a page");
        if used + h > room && !page.is_empty() {
            pages.push(Vec::new());
            used = 0.0;
        }
        pages.last_mut().expect("there is a page").push((piece, h));
        used += h;
    }
    pages
}

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
    let (screen, body, footer) = directions_frame(&layout);
    // A first pass measures the head and the rows, and splits them into pages.
    let mut plan = None;
    egui_pass.frame(gpu, screen, ppp, |ui| plan = Some(Plan::new(ui, doc, body)))?;
    let plan = plan.ok_or("the directions were not laid out")?;
    let total = plan.pages.len();
    for (n, pieces) in plan.pages.iter().enumerate() {
        let top = egui_pass.frame(gpu, screen, ppp, |ui| {
            let p = ui.painter().clone();
            // Nothing is drawn past the body, into the footer.
            ui.set_clip_rect(body);
            let mut y = body.top();
            for (i, &(piece, h)) in pieces.iter().enumerate() {
                let rect =
                    Rect::from_min_size(Pos2::new(body.left(), y), Vec2::new(body.width(), h));
                match piece {
                    Piece::Row(r) => plan.rows.paint(ui, rect, doc, r),
                    // A rule that would start a page has nothing above it to set apart.
                    Piece::Rule if i == 0 => {}
                    _ => draw_head_piece(ui, doc, &plan.head, piece, rect),
                }
                y += h;
            }
            p.text(
                footer.left_center(),
                Align2::LEFT_CENTER,
                format!("Page {} of {total}", n + 1),
                FontId::proportional(9.0),
                theme::PAPER_WEAK,
            );
            p.text(
                footer.right_center(),
                Align2::RIGHT_CENTER,
                CREDIT,
                FontId::proportional(9.0),
                theme::PAPER_WEAK,
            );
        })?;
        let mut rgba = blank(size_px);
        over(&mut rgba, &top);
        each(Page {
            size: size_px,
            size_pt: layout.page,
            rgb: rgb_of(rgba),
        })?;
    }
    Ok(total)
}

/// A directions page's screen, body and footer, in egui's units.
fn directions_frame(layout: &Layout) -> (Vec2, Rect, Rect) {
    let s = layout.text.factor();
    let to_u = |r: [f64; 4]| {
        Rect::from_min_max(
            Pos2::new((r[0] / s) as f32, (r[1] / s) as f32),
            Pos2::new((r[2] / s) as f32, (r[3] / s) as f32),
        )
    };
    let screen = Vec2::new(layout.page[0] as f32, layout.page[1] as f32) / s as f32;
    (screen, to_u(layout.map), to_u(layout.footer))
}

fn measure_head(ui: &egui::Ui, doc: &DirectionsDoc, width: f32) -> Head {
    let p = ui.painter();
    let label = (!doc.label.is_empty()).then(|| {
        let g = p.layout(
            doc.label.clone(),
            FontId::proportional(17.0),
            theme::PAPER_INK,
            width,
        );
        Rect::from_min_size(Pos2::ZERO, g.size())
    });
    let totals =
        p.layout_no_wrap(
            doc.totals.clone(),
            FontId::proportional(14.0),
            theme::PAPER_INK,
        )
        .size()
        .y + 4.0;
    let stops = doc
        .stops
        .iter()
        .map(|(_, name)| {
            p.layout(
                name.clone(),
                FontId::proportional(13.0),
                theme::PAPER_INK,
                width - 26.0,
            )
            .size()
            .y
            .max(18.0)
                + 2.0
        })
        .collect();
    // The last notice keeps a gap from the stops, as the totals do.
    let mut notices: Vec<f32> = doc
        .notices
        .iter()
        .map(|n| notice_galley(p, n, width).size().y + 2.0)
        .collect();
    if let Some(last) = notices.last_mut() {
        *last += GAP;
    }
    Head {
        label,
        totals,
        notices,
        stops,
    }
}

/// A notice as printed: "⚠" and its text, in the warning colour.
fn notice_galley(p: &egui::Painter, text: &str, width: f32) -> std::sync::Arc<egui::Galley> {
    p.layout(
        format!("⚠ {text}"),
        FontId::proportional(13.0),
        theme::PAPER_APPROX,
        width,
    )
}

/// One piece of the head in `rect`, its place on the page.
fn draw_head_piece(ui: &egui::Ui, doc: &DirectionsDoc, head: &Head, piece: Piece, rect: Rect) {
    let p = ui.painter();
    let (left, y) = (rect.left(), rect.top());
    match piece {
        Piece::Label if head.label.is_some() => {
            let g = p.layout(
                doc.label.clone(),
                FontId::proportional(17.0),
                theme::PAPER_INK,
                rect.width(),
            );
            p.galley(Pos2::new(left, y), g, theme::PAPER_INK);
        }
        Piece::Totals => {
            p.text(
                Pos2::new(left, y),
                Align2::LEFT_TOP,
                &doc.totals,
                FontId::proportional(14.0),
                theme::PAPER_INK,
            );
        }
        Piece::Notice(i) => {
            let Some(text) = doc.notices.get(i) else {
                return;
            };
            p.galley(
                Pos2::new(left, y),
                notice_galley(p, text, rect.width()),
                theme::PAPER_APPROX,
            );
        }
        Piece::Stop(i) => {
            let Some((fill, name)) = doc.stops.get(i) else {
                return;
            };
            let c = Pos2::new(left + 9.0, y + 9.0);
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
                theme::PAPER_INK,
                rect.width() - 26.0,
            );
            p.galley(Pos2::new(left + 24.0, y + 1.0), g, theme::PAPER_INK);
        }
        Piece::Rule => {
            p.hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(0.75, theme::PAPER_WEAK),
            );
        }
        Piece::Label | Piece::Row(_) => {}
    }
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
    fn a_piece_that_does_not_fit_starts_the_next_page() {
        let pieces = [
            (Piece::Totals, 60.0),
            (Piece::Rule, 30.0),
            (Piece::Row(0), 20.0),
        ];
        let pages = paginate(pieces.into_iter(), 100.0);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1], [(Piece::Row(0), 20.0)]);
        // Taller than a page: alone on its own.
        let pages = paginate(
            [(Piece::Row(0), 150.0), (Piece::Row(1), 10.0)].into_iter(),
            100.0,
        );
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn a_head_taller_than_a_page_goes_on_to_the_next() {
        // R7: 25 stops at 140% text on Letter fill more than the first page.
        use omp_core::route::Kind;
        let layout = Layout::new(print::Paper::Letter, false, false, TextSize(140));
        let (screen, body, _) = directions_frame(&layout);
        // Leg 1 has no route: a notice in the head, and a row among the steps.
        let steps: Vec<_> = (0..40)
            .map(|i| crate::routing::tests::step(Kind::Turn, i / 20 * 2, i..i + 1, 500.0))
            .collect();
        let doc = DirectionsDoc {
            label: "Twenty-five stops".into(),
            stops: (0..25)
                .map(|i| (Color32::RED, format!("Stop {i}, a road with a long name")))
                .collect(),
            totals: "1,234 mi · 20 h 5 min".into(),
            notices: vec!["No road goes from stop 2 to stop 3.".into()],
            gaps: vec![(1, "No route from stop 2 to stop 3".into())],
            step_times: vec![None; steps.len()],
            steps,
            notes: Vec::new(),
            note_times: Vec::new(),
            units: Units::Miles,
        };
        let ctx = egui::Context::default();
        theme::style(&ctx, false);
        let mut plan = None;
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| plan = Some(Plan::new(ui, &doc, body)));
            out.textures_delta.clear();
        }
        let plan = plan.expect("laid out");
        let head: f32 = plan
            .pages
            .iter()
            .flatten()
            .filter(|(p, _)| !matches!(p, Piece::Row(_)))
            .map(|p| p.1)
            .sum();
        assert!(head > body.height(), "{head} {}", body.height());
        for (n, page) in plan.pages.iter().enumerate() {
            let used: f32 = page.iter().map(|p| p.1).sum();
            assert!(
                used <= body.height(),
                "page {n}: {used} > {}",
                body.height()
            );
        }
        // Every piece once, in order: the label, the totals, the notice, the stops, the rule,
        // the rows.
        let all: Vec<Piece> = plan.pages.iter().flatten().map(|p| p.0).collect();
        let want: Vec<Piece> = [Piece::Label, Piece::Totals, Piece::Notice(0)]
            .into_iter()
            .chain((0..25).map(Piece::Stop))
            .chain([Piece::Rule])
            .chain((0..plan.rows.heights().len()).map(Piece::Row))
            .collect();
        assert_eq!(all, want);
        assert_eq!(plan.rows.heights().len(), 41, "40 steps and the gap");
        assert!(!plan.pages[0].iter().any(|p| matches!(p.0, Piece::Row(_))));
    }

    #[test]
    fn big_pages_are_given_less_dpi() {
        let l = Layout::new(print::Paper::Tabloid, false, false, TextSize::default());
        assert!(page_dpi(&l, 600.0) < 600.0);
        assert!(l.pixels(page_dpi(&l, 600.0))[1] <= 8192);
        let l = Layout::new(print::Paper::Letter, false, false, TextSize::default());
        assert_eq!(page_dpi(&l, 300.0), 300.0);
        assert_eq!(page_dpi(&l, 600.0), 600.0);
    }
}
