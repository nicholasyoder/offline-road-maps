//! The window: the map view, input, the debug overlay (F3, with tile outlines) and the pack
//! picker (Ctrl+O).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use eframe::egui_wgpu::{self, wgpu};

use crate::bench::{Bench, FrameSample, ReportContext};
use crate::cache::Cache;
use crate::camera::{Camera, Motion};
use crate::config::{self, Config, Location, View};
use crate::labels::{self, Placer, TileLabels};
use crate::loader::{self, Done, Loader};
use crate::mesh;
use crate::packs::{self, Opened};
use crate::render::{self, GpuMesh, MapCallback, Renderer};
use crate::style::{Style, StyleFile};
use crate::tiles::{self, Cover, Known, Resolved, TileId};

/// Tile cache budget: the tiles' vertex data, which lives on the GPU (phase1-plan.md 1.4).
const CACHE_BUDGET: usize = 96 << 20;
/// Shown where no tile is drawn: outside the pack, and tiles not loaded yet.
pub const NO_MAP: Color32 = Color32::from_rgb(222, 222, 216);
/// Keyboard pan speed, points per second.
const KEY_PAN: f64 = 700.0;
/// How often a changed view is written to `config.toml`.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// Tile timings kept for the overlay's averages.
const RECENT_TILES: usize = 256;

/// What the app was started with.
pub struct Startup {
    pub exe_dir: PathBuf,
    pub config: Location,
    /// First-chunk files found, and the one to open.
    pub found: Vec<PathBuf>,
    pub chosen: Option<PathBuf>,
    pub notices: Vec<String>,
    pub bench: Option<Bench>,
    pub started: Instant,
    /// MSAA samples the window was created with.
    pub msaa: u32,
    /// The style, and the file it comes from when that exists (`--style`, else `style.toml`
    /// next to the executable).
    pub style: StyleFile,
    pub style_path: PathBuf,
}

enum TileData {
    /// Not in the pack; drawn from its nearest stored ancestor.
    Absent,
    Mesh(Arc<GpuMesh>, Arc<TileLabels>, TileInfo),
    Error,
}

/// About a loaded tile, for the overlay.
#[derive(Clone, Copy)]
struct TileInfo {
    features: usize,
    vertices: usize,
    /// Compressed bytes read.
    bytes: u64,
    /// Polygons that could not be tessellated.
    failed: usize,
}

#[derive(Default)]
struct TileStats {
    loaded: usize,
    absent: usize,
    errors: usize,
    /// Polygons that could not be tessellated.
    failed: usize,
    last_error: Option<String>,
    /// Recent (latency, read, decode, prepare) in ms.
    recent: VecDeque<[f32; 4]>,
}

/// A tile as the loader threads prepare it.
type Prepared = (Arc<GpuMesh>, Arc<TileLabels>, TileInfo);

struct MapState {
    opened: Opened,
    style: Arc<Style>,
    loader: Loader<Prepared>,
    cache: Cache<TileData>,
    camera: Camera,
    motion: Motion,
    stats: TileStats,
    /// Set until the first frame, which gives the viewport size to fit the pack to.
    fit_on_first_frame: bool,
    view: FrameView,
    placer: Placer,
}

/// What the last frame showed, for the overlay.
#[derive(Default, Clone, Copy)]
struct FrameView {
    tile_zoom: u8,
    visible: usize,
    drawn: usize,
    fallbacks: usize,
    missing: usize,
    detailed: bool,
    /// Labels placed (shields among them), of how many candidates, and the time it took.
    labels: usize,
    shields: usize,
    candidates: usize,
    label_time: Duration,
    /// Labels are still fading in.
    fading: bool,
}

pub struct App {
    exe_dir: PathBuf,
    cfg_loc: Location,
    cfg: Config,
    found: Vec<PathBuf>,
    picker: bool,
    map: Option<MapState>,
    notices: Vec<String>,
    bench: Option<Bench>,
    started: Instant,
    first_full: Option<Duration>,
    last_frame: Option<Instant>,
    /// Recent frames: (start, interval ms, cpu ms).
    frames: VecDeque<(Instant, f32, f32)>,
    saved_view: Option<View>,
    last_save: Instant,
    cursor: Option<[f64; 2]>,
    adapter: String,
    device: Option<wgpu::Device>,
    srgb: bool,
    style: StyleFile,
    style_path: PathBuf,
    /// The style file's modification time when last read; debug builds reload it when it
    /// changes.
    style_mtime: Option<SystemTime>,
    style_checked: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, s: Startup) -> Self {
        let (cfg, err) = s.config.load();
        let mut notices = s.notices;
        notices.extend(s.config.fallback.clone());
        notices.extend(err);
        let rs = cc.wgpu_render_state.as_ref();
        let adapter = rs.map_or_else(String::new, |rs| {
            let i = rs.adapter.get_info();
            format!("{} ({:?})", i.name, i.backend)
        });
        if let Some(rs) = rs {
            let r = Renderer::new(&rs.device, rs.target_format, s.msaa);
            rs.renderer.write().callback_resources.insert(r);
        }
        let mut app = Self {
            exe_dir: s.exe_dir,
            cfg_loc: s.config,
            saved_view: cfg.view,
            cfg,
            picker: s.chosen.is_none() && s.found.len() > 1,
            found: s.found,
            map: None,
            notices,
            bench: s.bench,
            started: s.started,
            first_full: None,
            last_frame: None,
            frames: VecDeque::new(),
            last_save: Instant::now(),
            cursor: None,
            adapter,
            device: rs.map(|rs| rs.device.clone()),
            srgb: rs.is_some_and(|rs| rs.target_format.is_srgb()),
            style: s.style,
            style_mtime: mtime(&s.style_path),
            style_path: s.style_path,
            style_checked: Instant::now(),
        };
        if let Some(p) = s.chosen {
            app.open(&cc.egui_ctx, &p);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, first_chunk: &Path) {
        // Close the old pack first, so its loader threads stop.
        self.map = None;
        let opened = match packs::open(first_chunk) {
            Ok(o) => o,
            Err(e) => {
                self.notices.push(format!("Could not open the pack: {e}"));
                return;
            }
        };
        let dir = packs::pack_dir(first_chunk);
        let same_pack = self
            .cfg
            .pack
            .as_ref()
            .is_some_and(|p| config::resolve_path(p, &self.exe_dir) == dir);
        let mut camera = Camera::new(opened.bounds);
        let mut fit = true;
        if let (true, Some(v)) = (same_pack, self.cfg.view) {
            camera.set_lonlat(v.lon, v.lat, v.zoom);
            fit = !camera.contains_center();
        }
        let Some(device) = self.device.clone() else {
            self.notices
                .push("No GPU device: the map cannot be drawn.".into());
            return;
        };
        let style = compile_style(&self.style, &opened);
        let loader = start_loader(&opened, &style, device, ctx);
        self.cfg.pack = Some(config::store_path(&dir, &self.exe_dir));
        self.map = Some(MapState {
            opened,
            style,
            loader,
            cache: Cache::new(CACHE_BUDGET),
            camera,
            motion: Motion::default(),
            stats: TileStats::default(),
            fit_on_first_frame: fit,
            view: FrameView::default(),
            placer: Placer::default(),
        });
        self.first_full = None;
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map) {
            let w = b.flight.window;
            m.camera.size = [f64::from(w[0]), f64::from(w[1])];
            m.fit_on_first_frame = false;
            b.start(&mut m.camera, m.opened.reader.bytes_read());
        }
    }

    /// Debug builds: reads the style file again when it has changed, and redraws the map
    /// with it. Checked once a second.
    fn reload_style(&mut self, ctx: &egui::Context) {
        const EVERY: Duration = Duration::from_secs(1);
        if !cfg!(debug_assertions) || self.bench.is_some() {
            return;
        }
        ctx.request_repaint_after(EVERY);
        if self.style_checked.elapsed() < EVERY {
            return;
        }
        self.style_checked = Instant::now();
        let t = mtime(&self.style_path);
        if t.is_none() || t == self.style_mtime {
            return;
        }
        self.style_mtime = t;
        match StyleFile::read(&self.style_path) {
            Ok(f) => self.style = f,
            Err(e) => {
                self.notices.push(format!("Style not reloaded: {e}"));
                return;
            }
        }
        eprintln!("style: reloaded {}", self.style_path.display());
        if let (Some(m), Some(device)) = (&mut self.map, self.device.clone()) {
            // The meshes hold style IDs, so every tile is prepared again.
            m.style = compile_style(&self.style, &m.opened);
            m.loader = start_loader(&m.opened, &m.style, device, ctx);
            m.cache = Cache::new(CACHE_BUDGET);
            m.placer = Placer::default();
        }
    }

    fn save_config(&mut self) {
        if self.bench.is_some() {
            return;
        }
        if let Some(m) = &self.map {
            let [lon, lat] = m.camera.lonlat();
            self.cfg.view = Some(View {
                lat,
                lon,
                zoom: m.camera.zoom,
            });
        }
        if let Err(e) = self.cfg_loc.save(&self.cfg) {
            self.notices.push(format!(
                "Could not save {}: {e}",
                self.cfg_loc.path.display()
            ));
        }
        self.saved_view = self.cfg.view;
        self.last_save = Instant::now();
    }

    /// Saves a changed view once it has been still for a while; wakes the app for it.
    fn maybe_save(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.map else { return };
        if self.bench.is_some() || m.motion.moving() {
            return;
        }
        let [lon, lat] = m.camera.lonlat();
        let v = View {
            lat,
            lon,
            zoom: m.camera.zoom,
        };
        if self.saved_view == Some(v) {
            return;
        }
        match SAVE_EVERY.checked_sub(self.last_save.elapsed()) {
            Some(wait) if !wait.is_zero() => ctx.request_repaint_after(wait),
            _ => self.save_config(),
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn compile_style(file: &StyleFile, opened: &Opened) -> Arc<Style> {
    let (style, warnings) = Style::compile(file, &opened.schema);
    for w in warnings {
        eprintln!("{w}");
    }
    Arc::new(style)
}

/// Loader threads that read, tessellate with `style` and upload the pack's tiles.
fn start_loader(
    opened: &Opened,
    style: &Arc<Style>,
    device: wgpu::Device,
    ctx: &egui::Context,
) -> Loader<Prepared> {
    let ctx = ctx.clone();
    let style = style.clone();
    Loader::new(
        opened.reader.clone(),
        loader::default_threads(),
        move |id, tile| {
            let m = mesh::build(&tile, &style);
            let labels = labels::extract(id, &tile, &style);
            let info = TileInfo {
                features: tile.layers.iter().map(|(_, l)| l.features.len()).sum(),
                vertices: tile.layers.iter().map(|(_, l)| l.vertices()).sum(),
                bytes: tile.bytes,
                failed: m.failed,
            };
            (
                Arc::new(GpuMesh::upload(&device, &m)),
                Arc::new(labels),
                info,
            )
        },
        move || ctx.request_repaint(),
    )
}

impl MapState {
    fn take_results(&mut self, bench: &mut Option<Bench>) {
        let done: Vec<Done<_>> = self.loader.poll().collect();
        for d in done {
            if let Some(b) = bench {
                b.tile(&d);
            }
            let s = &mut self.stats;
            s.recent.push_back([
                d.latency.as_secs_f32() * 1e3,
                d.timing.read.as_secs_f32() * 1e3,
                d.timing.decode.as_secs_f32() * 1e3,
                d.prepare.as_secs_f32() * 1e3,
            ]);
            if s.recent.len() > RECENT_TILES {
                s.recent.pop_front();
            }
            let (data, bytes) = match d.result {
                Ok(Some((mesh, labels, info))) => {
                    s.loaded += 1;
                    s.failed += info.failed;
                    let bytes = mesh.bytes + labels.bytes;
                    (TileData::Mesh(mesh, labels, info), bytes)
                }
                Ok(None) => {
                    s.absent += 1;
                    (TileData::Absent, 0)
                }
                Err(e) => {
                    s.errors += 1;
                    s.last_error = Some(e.to_string());
                    (TileData::Error, 0)
                }
            };
            self.cache.insert(d.id, data, bytes + 64);
        }
    }

    fn input(&mut self, ui: &egui::Ui, resp: &egui::Response, origin: Pos2, dt: f64) {
        let cam = &mut self.camera;
        if resp.dragged() {
            let d = resp.drag_delta();
            cam.pan([f64::from(d.x), f64::from(d.y)]);
            self.motion.velocity = [0.0, 0.0];
        }
        if resp.drag_stopped() {
            let v = ui.input(|i| i.pointer.velocity());
            self.motion.velocity = [f64::from(v.x), f64::from(v.y)];
        }
        let center = [cam.size[0] / 2.0, cam.size[1] / 2.0];
        let anchor = resp.hover_pos().map_or(center, |p| {
            [f64::from(p.x - origin.x), f64::from(p.y - origin.y)]
        });
        if resp.double_clicked() {
            self.motion.zoom(1.0, anchor);
        }
        if resp.hovered() {
            ui.input(|i| {
                for e in &i.events {
                    match e {
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            let dz = match unit {
                                egui::MouseWheelUnit::Line => f64::from(delta.y) * 0.5,
                                egui::MouseWheelUnit::Point => f64::from(delta.y) / 200.0,
                                egui::MouseWheelUnit::Page => f64::from(delta.y) * 2.0,
                            };
                            self.motion.zoom(dz, anchor);
                        }
                        // Pinch.
                        egui::Event::Zoom(f) if *f > 0.0 => {
                            cam.zoom_around(f64::from(*f).log2(), anchor);
                        }
                        _ => {}
                    }
                }
            });
        }
        ui.input(|i| {
            if i.focused {
                let key = |k| if i.key_down(k) { 1.0 } else { 0.0 };
                let dx = key(egui::Key::ArrowLeft) - key(egui::Key::ArrowRight);
                let dy = key(egui::Key::ArrowUp) - key(egui::Key::ArrowDown);
                if dx != 0.0 || dy != 0.0 {
                    cam.pan([dx * KEY_PAN * dt, dy * KEY_PAN * dt]);
                    self.motion.velocity = [0.0, 0.0];
                }
                if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                    self.motion.zoom(1.0, center);
                }
                if i.key_pressed(egui::Key::Minus) {
                    self.motion.zoom(-1.0, center);
                }
            }
        });
    }

    fn keys_held(ui: &egui::Ui) -> bool {
        ui.input(|i| {
            [
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
            ]
            .into_iter()
            .any(|k| i.key_down(k))
        })
    }

    /// Picks, requests and draws the tiles for the current camera; with `outlines`, also the
    /// tile outlines and their numbers.
    fn draw(&mut self, painter: &egui::Painter, rect: Rect, ppp: f32, srgb: bool, outlines: bool) {
        let cam = &self.camera;
        self.cache.next_frame();
        let z = tiles::tile_zoom(cam.zoom);
        let sel = tiles::select(z, cam.visible(), self.opened.bounds);
        let cache = &self.cache;
        let known = |t| {
            cache.peek(t).map(|d| match d {
                TileData::Absent => Known::Absent,
                TileData::Mesh(..) | TileData::Error => Known::Loaded,
            })
        };
        let caps = self.style.caps_below(z);
        let plan = tiles::plan(&sel.visible, &caps, known);
        // The margin's next unread tiles, past the absent ones, so short pans find them ready.
        let margin: Vec<TileId> = sel
            .margin
            .iter()
            .filter_map(|&t| match tiles::resolve(t, known) {
                Resolved::Unknown(u) => Some(u),
                _ => None,
            })
            .collect();
        let mut wanted = plan.need.clone();
        for t in margin {
            if !wanted.contains(&t) {
                wanted.push(t);
            }
        }
        self.loader.request(&wanted);
        // Keep what this frame uses, absent tiles included, from being evicted.
        for &t in sel.visible.iter().chain(&sel.margin) {
            let mut at = Some(t);
            while let Some(a) = at {
                at = match self.cache.get(a) {
                    Some(TileData::Absent) => a.parent(),
                    _ => None,
                };
            }
        }
        let mut view = FrameView {
            tile_zoom: z,
            visible: sel.visible.len(),
            drawn: plan.draws.len(),
            detailed: plan.detailed(),
            ..Default::default()
        };
        let mut draws = Vec::with_capacity(plan.draws.len());
        for &(t, cover) in &plan.draws {
            match cover {
                Cover::Missing => view.missing += 1,
                Cover::Parent | Cover::Child => view.fallbacks += 1,
                Cover::Exact | Cover::Ancestor | Cover::Empty => {}
            }
            match (cover, self.cache.get(t)) {
                (Cover::Empty, _) => draws.push((t, None)),
                (_, Some(TileData::Mesh(m, ..))) => draws.push((t, Some(m.clone()))),
                _ => {}
            }
        }
        let mut capped = Vec::with_capacity(plan.capped.len());
        for &(t, cap, s) in &plan.capped {
            if let Some(TileData::Mesh(m, ..)) = self.cache.get(s) {
                capped.push((t, cap, s, m.clone()));
            }
        }
        let paints = render::paints(&self.style, cam.zoom, ppp, srgb);
        let mut frame = render::layout(cam, ppp, paints, draws, capped, &self.style.pass_cap);
        let t0 = Instant::now();
        let sources: Vec<labels::Source> = plan
            .labels
            .iter()
            .filter_map(|&(tile, square, cap)| match self.cache.peek(tile) {
                Some(TileData::Mesh(_, l, _)) => Some(labels::Source {
                    labels: l,
                    tile,
                    square,
                    cap,
                }),
                _ => None,
            })
            .collect();
        let placed = self
            .placer
            .place(cam, ppp, &self.style, &sources, srgb, Some(t0));
        frame.glyphs = placed.glyphs;
        frame.atlas = self.placer.atlas.take_uploads();
        view.labels = placed.labels;
        view.shields = placed.shields;
        view.candidates = placed.candidates;
        view.fading = placed.fading;
        view.label_time = t0.elapsed();
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            MapCallback(frame),
        ));
        self.view = view;
        if outlines {
            self.outlines(painter, rect.min, &plan.draws);
        }
    }

    fn outlines(&mut self, painter: &egui::Painter, origin: Pos2, plan: &[(TileId, Cover)]) {
        let cam = &self.camera;
        let to_screen = |w: [f64; 2]| {
            let p = cam.world_to_screen(w);
            Pos2::new(origin.x + p[0] as f32, origin.y + p[1] as f32)
        };
        let world_rect =
            |r: [f64; 4]| Rect::from_min_max(to_screen([r[0], r[1]]), to_screen([r[2], r[3]]));
        painter.rect_stroke(
            world_rect(self.opened.bounds),
            0.0,
            Stroke::new(2.0, Color32::from_rgb(90, 140, 200)),
            StrokeKind::Middle,
        );
        for &(t, cover) in plan {
            let r = world_rect(t.world_rect()).shrink(1.0);
            let data = self.cache.get(t);
            let stroke = match (cover, data) {
                (Cover::Missing, _) => Color32::from_gray(150),
                (_, Some(TileData::Error)) => Color32::from_rgb(200, 40, 40),
                (Cover::Parent, _) => Color32::from_rgb(220, 140, 30),
                (Cover::Child, _) => Color32::from_rgb(60, 160, 80),
                (Cover::Empty, _) => Color32::from_gray(200),
                (Cover::Ancestor, _) => Color32::from_rgb(40, 120, 200),
                (Cover::Exact, _) => Color32::from_rgb(70, 90, 120),
            };
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, stroke), StrokeKind::Inside);
            if matches!(cover, Cover::Exact | Cover::Ancestor | Cover::Empty) && r.width() > 150.0 {
                let text = match data {
                    Some(TileData::Mesh(m, _, i)) => {
                        let failed = match i.failed {
                            0 => String::new(),
                            n => format!(", {n} failed"),
                        };
                        format!(
                            "{t}\n{} features, {} vertices{failed}\n{:.1} KiB read, {:.0} KiB mesh",
                            i.features,
                            i.vertices,
                            i.bytes as f64 / 1024.0,
                            m.bytes as f64 / 1024.0
                        )
                    }
                    _ if cover == Cover::Empty => format!("{t}\nnothing stored"),
                    Some(TileData::Error) => format!("{t}\nerror"),
                    Some(TileData::Absent) => format!("{t}\nabsent"),
                    None => t.to_string(),
                };
                painter.text(
                    r.min + Vec2::new(6.0, 6.0),
                    Align2::LEFT_TOP,
                    text,
                    FontId::monospace(11.0),
                    Color32::from_gray(40),
                );
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let interval = self.last_frame.map_or(Duration::ZERO, |l| t0 - l);
        self.last_frame = Some(t0);
        let dt = interval.as_secs_f64().min(0.1);
        let ctx = ui.ctx().clone();
        self.reload_style(&ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::F3)) {
            self.cfg.debug_overlay = !self.cfg.debug_overlay;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) && self.bench.is_none()
        {
            self.picker = !self.picker;
        }

        let mut map_rect = Rect::NOTHING;
        let mut animate = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(NO_MAP))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                map_rect = rect;
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                let Some(m) = &mut self.map else {
                    let msg = if self.found.is_empty() {
                        format!(
                            "No map pack found.\nPut a pack in {} or start with --pack <folder>.",
                            self.exe_dir.join("maps").display()
                        )
                    } else {
                        "Choose a pack (Ctrl+O).".into()
                    };
                    painter.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        msg,
                        FontId::proportional(16.0),
                        Color32::DARK_GRAY,
                    );
                    return;
                };
                m.camera.size = [f64::from(rect.width()), f64::from(rect.height())];
                if m.fit_on_first_frame {
                    m.camera.fit_bounds();
                    m.fit_on_first_frame = false;
                }
                m.camera.clamp();
                m.take_results(&mut self.bench);
                if self.bench.is_none() {
                    m.input(ui, &resp, rect.min, dt);
                    m.motion.step(&mut m.camera, dt);
                    animate = m.motion.moving() || MapState::keys_held(ui) || m.view.fading;
                    self.cursor = resp.hover_pos().map(|p| {
                        let w = m.camera.screen_to_world([
                            f64::from(p.x - rect.min.x),
                            f64::from(p.y - rect.min.y),
                        ]);
                        crate::camera::world_to_lonlat(w)
                    });
                }
                let outlines = self.cfg.debug_overlay;
                m.draw(&painter, rect, ctx.pixels_per_point(), self.srgb, outlines);
                if m.view.detailed && self.first_full.is_none() {
                    self.first_full = Some(self.started.elapsed());
                }
            });

        if self.cfg.debug_overlay {
            self.overlay(&ctx, map_rect);
        }
        self.picker_window(&ctx);
        self.notices_window(&ctx);

        let cpu = t0.elapsed();
        self.frames
            .push_back((t0, interval.as_secs_f32() * 1e3, cpu.as_secs_f32() * 1e3));
        while self
            .frames
            .front()
            .is_some_and(|f| t0 - f.0 > Duration::from_secs(2))
        {
            self.frames.pop_front();
        }

        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map) {
            let sample = FrameSample {
                interval,
                cpu,
                detailed: m.view.detailed,
                cache_bytes: m.cache.bytes(),
                labels: m.view.label_time,
                bytes_read: m.opened.reader.bytes_read(),
            };
            let rc = ReportContext {
                pack: m.opened.first_chunk.display().to_string(),
                adapter: self.adapter.clone(),
                pixels_per_point: ctx.pixels_per_point(),
                cache_budget: m.cache.budget,
            };
            if let Some(report) = b.end_frame(sample, &mut m.camera, &rc) {
                eprint!("{}", report.summary());
                let out = b.out().to_path_buf();
                match serde_json::to_string_pretty(&report)
                    .map_err(std::io::Error::other)
                    .and_then(|j| std::fs::write(&out, j + "\n"))
                {
                    Ok(()) => eprintln!("report written to {}", out.display()),
                    Err(e) => eprintln!("could not write {}: {e}", out.display()),
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            animate = true;
        } else if self.bench.is_some() {
            eprintln!("bench: no pack is open");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if animate {
            ctx.request_repaint();
        }
        self.maybe_save(&ctx);
    }

    fn on_exit(&mut self) {
        self.save_config();
    }
}

impl App {
    fn overlay(&self, ctx: &egui::Context, map_rect: Rect) {
        let mut lines = Vec::new();
        let n = self.frames.len().max(1) as f32;
        let span = self
            .frames
            .back()
            .zip(self.frames.front())
            .map_or(0.0, |(b, f)| (b.0 - f.0).as_secs_f32());
        let fps = if span > 0.0 { (n - 1.0) / span } else { 0.0 };
        let mut cpu: Vec<f32> = self.frames.iter().map(|f| f.2).collect();
        cpu.sort_by(f32::total_cmp);
        let p95 = cpu
            .get(((cpu.len() as f32 - 1.0) * 0.95).round() as usize)
            .copied()
            .unwrap_or(0.0);
        let last = self.frames.back().map_or((0.0, 0.0), |f| (f.1, f.2));
        lines.push(format!(
            "{fps:5.1} fps   frame {:5.2} ms   cpu {:5.2} ms (p95 {p95:.2})",
            last.0, last.1
        ));
        if let Some(m) = &self.map {
            let [lon, lat] = m.camera.lonlat();
            let v = m.view;
            lines.push(format!(
                "zoom {:.2} (tiles z{})   {lat:.5}, {lon:.5}",
                m.camera.zoom, v.tile_zoom
            ));
            if let Some([lon, lat]) = self.cursor {
                lines.push(format!("cursor {lat:.5}, {lon:.5}"));
            }
            let (queued, reading) = m.loader.pending();
            lines.push(format!(
                "tiles: {} visible, {} drawn ({} fallback, {} missing), {queued} queued, {reading} reading",
                v.visible, v.drawn, v.fallbacks, v.missing
            ));
            lines.push(format!(
                "labels: {} placed ({} shields) of {} candidates in {:.2} ms, {} sprites in the atlas ({:.0}% full)",
                v.labels,
                v.shields,
                v.candidates,
                v.label_time.as_secs_f64() * 1e3,
                m.placer.atlas.len(),
                m.placer.atlas.fill() * 100.0
            ));
            lines.push(format!(
                "cache: {} tiles, {:.1} / {:.0} MiB",
                m.cache.len(),
                mib(m.cache.bytes() as u64),
                mib(m.cache.budget as u64)
            ));
            let s = &m.stats;
            let k = s.recent.len().max(1) as f32;
            let avg = |i: usize| s.recent.iter().map(|r| r[i]).sum::<f32>() / k;
            lines.push(format!(
                "loaded {} + {} absent, {} errors, {} polygons failed   last {}: latency {:.2} ms, read {:.2} ms, decode {:.2} ms, tessellate {:.2} ms",
                s.loaded,
                s.absent,
                s.errors,
                s.failed,
                s.recent.len(),
                avg(0),
                avg(1),
                avg(2),
                avg(3)
            ));
            lines.push(format!(
                "read from pack: {:.1} MiB",
                mib(m.opened.reader.bytes_read())
            ));
            if let Some(e) = &s.last_error {
                lines.push(format!("last error: {e}"));
            }
        }
        if let Some(u) = crate::memory::usage() {
            lines.push(format!(
                "RSS {:.0} MiB (peak {:.0})",
                mib(u.rss),
                mib(u.peak)
            ));
        }
        if let Some(t) = self.first_full {
            lines.push(format!(
                "first full frame after {:.0} ms",
                t.as_secs_f64() * 1e3
            ));
        }
        if !self.adapter.is_empty() {
            lines.push(self.adapter.clone());
        }
        egui::Area::new(egui::Id::unique("debug-overlay"))
            .fixed_pos(map_rect.min + Vec2::new(8.0, 8.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(lines.join("\n")).monospace().size(12.0));
                });
            });
    }

    fn picker_window(&mut self, ctx: &egui::Context) {
        if !self.picker {
            return;
        }
        let mut open = true;
        let mut chosen = None;
        egui::Window::new("Open a pack")
            .open(&mut open)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if self.found.is_empty() {
                    ui.label("No packs found.");
                }
                for p in &self.found {
                    let dir = packs::pack_dir(p);
                    let shown = config::store_path(&dir, &self.exe_dir);
                    let current = self
                        .map
                        .as_ref()
                        .is_some_and(|m| &m.opened.first_chunk == p);
                    if ui
                        .selectable_label(current, shown.display().to_string())
                        .clicked()
                    {
                        chosen = Some(p.clone());
                    }
                }
            });
        if let Some(p) = chosen {
            self.open(ctx, &p);
            open = false;
        }
        self.picker = open;
    }

    fn notices_window(&mut self, ctx: &egui::Context) {
        if self.notices.is_empty() {
            return;
        }
        let mut dismiss = false;
        egui::Window::new("Notice")
            .collapsible(false)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
            .show(ctx, |ui| {
                for n in &self.notices {
                    ui.label(n);
                }
                dismiss = ui.button("OK").clicked();
            });
        if dismiss {
            self.notices.clear();
        }
    }
}

fn mib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}
