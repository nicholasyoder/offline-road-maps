//! The window: the side panel on the left (search, "what's here", the route, trips, layers
//! and the ☰ menu; phase 3 step 3.8), the map beside it with its input, the controls over the
//! map (scale bar, cursor coordinates, zoom buttons, credit line, and a button to open the
//! panel when it is closed), the dialogs (pack picker, About this map, Verify pack, errors,
//! and saving a trip and the file browser, phase 4 step 4.7) and the debug overlay (F3, with
//! tile outlines). Saved trips and pushpins live next to the executable and outlast the
//! pack: they belong to the app, not to the open map.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use eframe::egui_wgpu::{self, wgpu};

use crate::bench::{
    Bench, FrameSample, OptimizeReport, Poll, ReportContext, RouteReport, SearchContext,
};
use crate::cache::Cache;
use crate::camera::{Camera, Motion};
use crate::config::{self, Config, Location, View};
use crate::here::{HereUi, lonlat_text};
use crate::labels::{self, Placer, TileLabels};
use crate::layers::{Hidden, Layers};
use crate::loader::{self, Done, Loader};
use crate::mesh;
use crate::packs::{self, Opened};
use crate::pins::{self, PinRef, PinsUi};
use crate::pool::Pool;
use crate::printui::{self, PrintUi};
use crate::render::{self, GpuMesh, MapCallback, Renderer};
use crate::routing::{Place, RouteUi, StopAction};
use crate::scale::{self, Units};
use crate::search::{self, SearchUi, Status};
use crate::side::{self, Action, Page};
use crate::style::{Style, StyleFile};
use crate::theme::{Glyph, INK, glyph_button};
use crate::tiles::{self, Cover, Known, Resolved, TileId};
use crate::trips::{self, Browsed, Named, TripFile, TripsUi};
use crate::verify::{Outcome, Verify};

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
    /// What the layer toggles hide, and the toggles it was made for.
    hidden: Hidden,
    hidden_for: Option<Layers>,
    /// The scale bar's units unless the config sets them.
    units: Units,
    /// A tile failed to load and "Verify pack" was suggested.
    error_suggested: bool,
    search: SearchUi,
    here: HereUi,
    routing: RouteUi,
    /// The side panel's page.
    page: Page,
    /// Where the map was last drawn, to keep it still when the side panel opens, closes or
    /// is resized.
    last_rect: Rect,
    /// `OMP_BENCH_NO_MAP`: everything but drawing the map, to see what the rest costs.
    no_map: bool,
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
    /// The config as last saved.
    saved: Config,
    last_save: Instant,
    cursor: Option<[f64; 2]>,
    adapter: String,
    device: Option<wgpu::Device>,
    /// Tile geometry on the GPU.
    pool: Option<Arc<Pool>>,
    srgb: bool,
    style: StyleFile,
    style_path: PathBuf,
    /// The style file's modification time when last read; debug builds reload it when it
    /// changes.
    style_mtime: Option<SystemTime>,
    style_checked: Instant,
    /// A pack that could not be opened, and why.
    pack_error: Option<String>,
    /// A tile could not be read; offer to verify the pack.
    tile_error: Option<String>,
    verify: Option<Verify>,
    about: bool,
    /// The picker's folder field.
    path_input: String,
    /// Saved trips and pushpins (phase 4 step 4.7).
    trips: TripsUi,
    pins: PinsUi,
    /// The print page and its preview (phase 4 step 4.9).
    print: PrintUi,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, s: Startup) -> Self {
        let (cfg, err) = s.config.load();
        let mut notices = s.notices;
        notices.extend(s.config.fallback.clone());
        notices.extend(err);
        crate::theme::apply(&cc.egui_ctx);
        let rs = cc.wgpu_render_state.as_ref();
        let adapter = rs.map_or_else(String::new, |rs| {
            let i = rs.adapter.get_info();
            format!("{} ({:?})", i.name, i.backend)
        });
        if s.bench.is_some() {
            eprintln!("adapter: {adapter}");
        }
        if let Some(rs) = rs {
            let r = Renderer::new(&rs.device, rs.target_format, s.msaa);
            rs.renderer.write().callback_resources.insert(r);
        }
        let pins = match s.bench {
            // The bench draws its own pins, or none, whatever is next to it.
            Some(_) => PinsUi::empty(s.exe_dir.join(pins::DIR)),
            None => PinsUi::load(&s.exe_dir),
        };
        let mut app = Self {
            trips: TripsUi::new(&s.exe_dir),
            pins,
            print: PrintUi::new(),
            exe_dir: s.exe_dir,
            cfg_loc: s.config,
            saved: cfg.clone(),
            cfg,
            // With no pack to open, the picker, whose folder field can find one.
            picker: s.chosen.is_none() && s.bench.is_none(),
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
            pool: rs.map(|rs| Pool::new(rs.device.clone(), rs.queue.clone())),
            srgb: rs.is_some_and(|rs| rs.target_format.is_srgb()),
            style: s.style,
            style_mtime: mtime(&s.style_path),
            style_path: s.style_path,
            style_checked: Instant::now(),
            pack_error: None,
            tile_error: None,
            verify: None,
            about: false,
            path_input: String::new(),
        };
        if let Some(p) = s.chosen {
            app.open(&cc.egui_ctx, &p);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, first_chunk: &Path) {
        // Close the old pack first, so its loader threads and a check of it stop.
        self.map = None;
        self.verify = None;
        self.tile_error = None;
        let opened = match packs::open(first_chunk) {
            Ok(o) => o,
            Err(e) => {
                self.pack_error = Some(e);
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
        let Some(pool) = self.pool.clone() else {
            self.notices
                .push("No GPU device: the map cannot be drawn.".into());
            return;
        };
        let style = compile_style(&self.style, &opened);
        let loader = start_loader(&opened, &style, pool, ctx);
        self.cfg.pack = Some(config::store_path(&dir, &self.exe_dir));
        let units = packs::default_units(opened.meta.as_ref().ok());
        let search = SearchUi::start(opened.first_chunk.clone(), &opened.schema, ctx);
        let mut routing = RouteUi::start(opened.first_chunk.clone(), ctx);
        routing.plan.s = self.cfg.schedule.clone();
        routing.plan.time = self.cfg.schedule.depart.clone();
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
            hidden: Hidden::default(),
            hidden_for: None,
            units,
            error_suggested: false,
            search,
            here: HereUi::default(),
            routing,
            page: Page::default(),
            last_rect: Rect::NOTHING,
            no_map: std::env::var_os("OMP_BENCH_NO_MAP").is_some(),
        });
        self.first_full = None;
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map) {
            // The panel as it first opens, whatever the config says.
            self.cfg.side_panel = config::SidePanel::default();
            let w = b.flight.window;
            m.camera.size = [f64::from(w[0]), f64::from(w[1])];
            m.fit_on_first_frame = false;
            b.start(&mut m.camera, m.opened.reader.bytes_read());
            // Checked when the flight was loaded.
            set_stops(&mut m.routing, &b.flight.route_stops().unwrap_or_default());
            if let Some(n) = b.flight.pins {
                self.pins.replace(vec![pins::bench_set(n, m.opened.bounds)]);
            }
            if let Some((date, time)) = b.flight.depart.as_deref().and_then(|d| d.split_once('T')) {
                m.routing.plan.s = config::Schedule::default();
                m.routing.plan.date = date.into();
                m.routing.plan.time = time.into();
                m.routing.plan.mode = crate::routing::Mode::Depart;
                m.routing.times_open = true;
                m.routing.retime();
            }
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
        if let (Some(m), Some(pool)) = (&mut self.map, self.pool.clone()) {
            // The meshes hold style IDs, so every tile is prepared again.
            m.style = compile_style(&self.style, &m.opened);
            m.loader = start_loader(&m.opened, &m.style, pool, ctx);
            m.cache = Cache::new(CACHE_BUDGET);
            m.placer = Placer::default();
            m.hidden_for = None;
        }
    }

    /// Puts the camera's view into the config.
    fn update_view(&mut self) {
        if let Some(m) = &self.map {
            let [lon, lat] = m.camera.lonlat();
            self.cfg.view = Some(View {
                lat,
                lon,
                zoom: m.camera.zoom,
            });
        }
    }

    fn save_config(&mut self) {
        if self.bench.is_some() {
            return;
        }
        self.update_view();
        if let Err(e) = self.cfg_loc.save(&self.cfg) {
            self.notices.push(format!(
                "Could not save {}: {e}",
                self.cfg_loc.path.display()
            ));
        }
        self.saved = self.cfg.clone();
        self.last_save = Instant::now();
    }

    /// Saves a changed config (view, pack, toggles, units) once the map has been still for
    /// a while; wakes the app for it.
    fn maybe_save(&mut self, ctx: &egui::Context) {
        if self.bench.is_some() || self.map.as_ref().is_some_and(|m| m.motion.moving()) {
            return;
        }
        self.update_view();
        if self.cfg == self.saved {
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
    pool: Arc<Pool>,
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
            (Arc::new(GpuMesh::upload(&pool, &m)), Arc::new(labels), info)
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

    fn input(&mut self, ui: &egui::Ui, resp: &egui::Response, origin: Pos2, dt: f64, pan: bool) {
        let cam = &mut self.camera;
        if resp.dragged() && pan {
            let d = resp.drag_delta();
            cam.pan([f64::from(d.x), f64::from(d.y)]);
            self.motion.stop_flying();
            self.motion.velocity = [0.0, 0.0];
        }
        if resp.drag_stopped() && pan {
            let v = ui.input(|i| i.pointer.velocity());
            self.motion.velocity = [f64::from(v.x), f64::from(v.y)];
        }
        let center = [cam.size[0] / 2.0, cam.size[1] / 2.0];
        let anchor = resp.hover_pos().map_or(center, |p| {
            [f64::from(p.x - origin.x), f64::from(p.y - origin.y)]
        });
        if resp.double_clicked() && pan {
            self.motion.zoom(1.0, anchor);
        }
        if resp.hovered() {
            ui.input(|i| {
                for e in &i.events {
                    match e {
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            let dz = match unit {
                                egui::MouseWheelUnit::Line => f64::from(delta.y) * 0.25,
                                egui::MouseWheelUnit::Point => f64::from(delta.y) / 400.0,
                                egui::MouseWheelUnit::Page => f64::from(delta.y),
                            };
                            self.motion.zoom(dz, anchor);
                        }
                        // Pinch.
                        egui::Event::Zoom(f) if *f > 0.0 => {
                            self.motion.stop_flying();
                            cam.zoom_around(f64::from(*f).log2(), anchor);
                        }
                        _ => {}
                    }
                }
            });
        }
        let typing = ui.ctx().egui_wants_keyboard_input();
        ui.input(|i| {
            if i.focused && !typing {
                let key = |k| if i.key_down(k) { 1.0 } else { 0.0 };
                let dx = key(egui::Key::ArrowLeft) - key(egui::Key::ArrowRight);
                let dy = key(egui::Key::ArrowUp) - key(egui::Key::ArrowDown);
                if dx != 0.0 || dy != 0.0 {
                    cam.pan([dx * KEY_PAN * dt, dy * KEY_PAN * dt]);
                    self.motion.stop_flying();
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

    /// Picks, requests and draws the tiles and the route for the current camera, with what
    /// `layers` shows; with `outlines`, also the tile outlines and their numbers.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        ppp: f32,
        srgb: bool,
        layers: &Layers,
        outlines: bool,
        pool: &Arc<Pool>,
        pins: &mut PinsUi,
    ) {
        if self.hidden_for.as_ref() != Some(layers) {
            self.hidden = Hidden::new(layers, &self.style, &self.opened.schema);
            self.hidden_for = Some(*layers);
        }
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
        let mut seen: HashSet<TileId> = wanted.iter().copied().collect();
        wanted.extend(margin.into_iter().filter(|&t| seen.insert(t)));
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
        let paints = render::paints(&self.style, &self.hidden, cam.zoom, ppp, srgb);
        let mut frame = render::layout(cam, ppp, paints, draws, capped, &self.style.pass_cap);
        frame.route = self.routing.layout(pool, cam, ppp);
        frame.pins = pins.layout(pool, cam, ppp);
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
        let placed = self.placer.place(
            cam,
            ppp,
            &self.style,
            &self.hidden,
            &sources,
            srgb,
            Some(t0),
        );
        frame.glyphs = placed.glyphs;
        frame.atlas = self.placer.atlas.take_uploads();
        view.labels = placed.labels;
        view.shields = placed.shields;
        view.candidates = placed.candidates;
        view.fading = placed.fading;
        view.label_time = t0.elapsed();
        if !self.no_map {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                MapCallback(frame),
            ));
        }
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

        self.side_panel(ui);

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
                        rect.center_top() + Vec2::new(0.0, 60.0),
                        Align2::CENTER_TOP,
                        msg,
                        FontId::proportional(16.0),
                        Color32::DARK_GRAY,
                    );
                    return;
                };
                m.camera.size = [f64::from(rect.width()), f64::from(rect.height())];
                keep_still(&mut m.camera, m.last_rect, rect);
                m.last_rect = rect;
                if m.fit_on_first_frame {
                    m.camera.fit_bounds();
                    m.fit_on_first_frame = false;
                }
                m.camera.clamp();
                m.take_results(&mut self.bench);
                if m.stats.errors > 0 && !m.error_suggested && self.bench.is_none() {
                    m.error_suggested = true;
                    self.tile_error = m.stats.last_error.clone();
                }
                if let Some(stops) = self.bench.as_mut().and_then(Bench::route_ask) {
                    set_stops(&mut m.routing, &stops);
                }
                let waiting = m.routing.waiting.is_some();
                let looking = m.routing.looking();
                let fit = m.routing.take(&m.camera);
                if let (Some(b), true, false) = (&mut self.bench, looking, m.routing.looking())
                    && let Some(t) = m.routing.alt_taken
                {
                    b.alternatives_shown(m.routing.found(), t.latency.as_secs_f64() * 1e3);
                }
                match (&mut self.bench, fit) {
                    (None, Some((c, z))) => m.motion.fly_to(&m.camera, c, z),
                    (Some(b), _) if waiting && m.routing.waiting.is_none() => {
                        let r = &m.routing;
                        if b.optimize_pending() {
                            b.optimize_answer(match (&r.last, &r.route) {
                                (Some(t), Some(s)) => Ok(OptimizeReport {
                                    latency_ms: 0.0,
                                    order_ms: t.optimize.as_secs_f64() * 1e3,
                                    km: s.trip.metres() / 1000.0,
                                    minutes: s.trip.time_s() / 60.0,
                                }),
                                _ => Err("no route"),
                            });
                        } else {
                            b.route_answer(match (&r.last, &r.route, &r.error) {
                                (Some(t), Some(s), _) => Ok(RouteReport {
                                    stops: s.trip.snaps.len(),
                                    latency_ms: t.latency.as_secs_f64() * 1e3,
                                    trip_ms: t.trip.as_secs_f64() * 1e3,
                                    directions_ms: t.directions.as_secs_f64() * 1e3,
                                    steps: s.steps.len(),
                                    km: s.trip.metres() / 1000.0,
                                    minutes: s.trip.time_s() / 60.0,
                                    points: t.points,
                                    alternatives: None,
                                    alternatives_ms: None,
                                }),
                                (_, _, Some(e)) => Err(e.clone()),
                                _ => Err("no route".into()),
                            });
                        }
                        if b.flight.alternatives {
                            m.routing.expand_all();
                        }
                    }
                    _ => {}
                }
                if let Some(b) = &mut self.bench {
                    if b.optimize_ask() {
                        let why = m.routing.optimize_blocked();
                        m.routing.optimize();
                        if m.routing.waiting.is_none() {
                            b.optimize_answer(Err(why.unwrap_or("not asked")));
                        }
                    }
                    if b.alternatives_ask() {
                        m.routing.expand_all();
                    }
                }
                if self.bench.is_none() {
                    // The print page's rectangle takes a drag that starts on it.
                    let held = m.page == Page::Print
                        && self.print.shows_rectangle()
                        && self
                            .print
                            .pointer(&ctx, &resp, rect.min, &m.camera, &self.cfg.print);
                    m.input(ui, &resp, rect.min, dt, !held);
                    // A click on a pushpin opens it; on an alternative's grey line or tag, picks
                    // it.
                    if resp.clicked()
                        && !held
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let at = [f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y)];
                        match self.pins.hit(&m.camera, at) {
                            Some(r) => self.pins.show(r),
                            None => {
                                m.routing.click(&m.camera, at);
                            }
                        }
                    }
                    let menu = m.routing.menu_state();
                    let picked = m
                        .here
                        .map_input(&resp, rect.min, &m.camera, &m.search, &menu);
                    if let Some((a, at)) = picked {
                        let place = marker_at(&m.search, &m.camera, at)
                            .map_or_else(|| Place::at(at), Place::of_hit);
                        let unnamed = place.name.is_none() && a != StopAction::Clear;
                        m.routing.apply(a, place);
                        if unnamed {
                            m.here.ask_stop(&m.search, at);
                        }
                    }
                    m.motion.step(&mut m.camera, dt);
                    animate = m.motion.moving() || MapState::keys_held(ui) || m.view.fading;
                    self.cursor = resp.hover_pos().map(|p| {
                        let w = m.camera.screen_to_world([
                            f64::from(p.x - rect.min.x),
                            f64::from(p.y - rect.min.y),
                        ]);
                        omp_core::geo::unit_to_lonlat(w)
                    });
                }
                // The bench draws every layer and no overlay, whatever the config says.
                let outlines = self.cfg.debug_overlay && self.bench.is_none();
                let layers = match self.bench {
                    Some(_) => Layers::default(),
                    None => self.cfg.layers,
                };
                if let Some(pool) = &self.pool {
                    m.draw(
                        &painter,
                        rect,
                        ctx.pixels_per_point(),
                        self.srgb,
                        &layers,
                        outlines,
                        pool,
                        &mut self.pins,
                    );
                }
                self.pins.draw_names(&painter, rect.min, &m.camera);
                if let Some(h) = &m.search.session.marker {
                    search::draw_marker(&painter, rect.min, &m.camera, h);
                }
                m.routing.draw_markers(&painter, rect.min, &m.camera);
                m.here.draw_marker(&painter, rect.min, &m.camera);
                if m.page == Page::Print && self.print.shows_rectangle() {
                    self.print
                        .draw_rectangle(&painter, rect, &m.camera, &self.cfg.print);
                }
                if m.view.detailed && self.first_full.is_none() {
                    self.first_full = Some(self.started.elapsed());
                }
            });

        let mut close_print = false;
        if let (Some(b), Some(m)) = (&mut self.bench, &mut self.map)
            && let Some(kind) = b.print_open()
        {
            // As the Print tab opens it.
            self.print.back = m.page;
            m.page = Page::Print;
            let around = printui::Around {
                cam: &m.camera,
                units: Units::Miles,
                has_route: m.routing.route.is_some(),
                route_bounds: m.routing.route_bounds(),
            };
            self.print.open(&ctx, kind, &around, &self.cfg.print);
        }
        if let Some(m) = &self.map
            && m.page == Page::Print
        {
            let inputs = printui::Inputs {
                pack: &m.opened.first_chunk,
                style: &self.style,
                layers: self.cfg.layers,
                units: self.cfg.units.unwrap_or(m.units),
                msaa: render::samples(self.cfg.msaa),
                settings: self.cfg.print.clone(),
                routing: &m.routing,
                pins: &self.pins,
            };
            self.print.tick(&ctx, &inputs);
            self.print.big_window(&ctx);
            if let Some(b) = &mut self.bench {
                if b.print_previewing()
                    && let Some(r) = self.print.bench_previewed()
                {
                    b.print_previewed(r);
                }
                if let Some((path, dpi)) = b.print_output() {
                    self.print.bench_start(&ctx, &inputs, path, dpi);
                }
                if b.print_running()
                    && let Some(r) = self.print.bench_finished()
                {
                    b.print_done(r);
                    close_print = true;
                }
            }
        }
        if close_print && let Some(m) = &mut self.map {
            m.page = self.print.back;
        }
        if self.cfg.debug_overlay && self.bench.is_none() {
            self.overlay(&ctx, map_rect);
        }
        self.map_controls(&ctx, map_rect);
        self.picker_window(&ctx);
        self.about_window(&ctx);
        self.verify_window(&ctx);
        self.error_windows(&ctx);
        self.notices_window(&ctx);
        self.trip_windows(&ctx);

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

        if let Some(path) = self.bench.as_mut().and_then(Bench::take_shot) {
            ctx.request_screenshot(move |img| {
                let size = img.size.map(|v| v as u32);
                if let Err(e) = crate::snapshot::write_png(&path, size, img.as_raw()) {
                    eprintln!("could not write {}: {e}", path.display());
                }
            });
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
            let st = m.search.stats();
            let rc = ReportContext {
                pack: m.opened.first_chunk.display().to_string(),
                adapter: self.adapter.clone(),
                pixels_per_point: ctx.pixels_per_point(),
                cache_budget: m.cache.budget,
                search: (m.search.status() == Status::Ready).then(|| SearchContext {
                    open_ms: st.open.map(|d| d.as_secs_f64() * 1e3),
                    heap_bytes: st.heap_bytes,
                    bytes_read: st.bytes_read,
                }),
            };
            if let Some(report) = b.end_frame(sample, &mut m.camera, &rc) {
                b.start_idle(report);
            }
            match b.poll() {
                Poll::Flying => animate = true,
                Poll::Idle(wait) => ctx.request_repaint_after(wait),
                Poll::Done(mut report) => {
                    if let Some(r) = self
                        .device
                        .as_ref()
                        .and_then(|d| d.generate_allocator_report())
                    {
                        report.gpu_allocated = Some(r.total_allocated_bytes);
                        report.gpu_reserved = Some(r.total_reserved_bytes);
                    }
                    report.tile_slabs_peak = self.pool.as_ref().map(|p| p.bytes().1);
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
            }
        } else if self.bench.is_some() {
            eprintln!("bench: no pack is open");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if animate {
            ctx.request_repaint();
        }
        if let Some(p) = &self.pool {
            p.end_frame();
        }
        self.maybe_save(&ctx);
    }

    fn on_exit(&mut self) {
        self.pins.flush();
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
            lines.extend(m.search.overlay_lines());
            lines.extend(m.routing.overlay_lines());
            lines.extend(m.here.overlay_line());
        }
        lines.push(format!(
            "pushpins: {} in {} sets",
            self.pins.count(),
            self.pins.sets.len()
        ));
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
        // Below the button that opens the side panel, when it is closed.
        let top = if self.cfg.side_panel.open { 8.0 } else { 48.0 };
        egui::Area::new(egui::Id::unique("debug-overlay"))
            .fixed_pos(map_rect.min + Vec2::new(8.0, top))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .shadow(crate::theme::MAP_SHADOW)
                    .show(ui, |ui| {
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
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if self.found.is_empty() {
                    ui.label(format!(
                        "No packs found in {}.",
                        self.exe_dir.join("maps").display()
                    ));
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
                ui.separator();
                ui.label("Or open the pack in a folder:");
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("folder or .omp.000 file")
                            .desired_width(320.0),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Open").clicked() || enter) && !self.path_input.trim().is_empty()
                    {
                        let dir = PathBuf::from(self.path_input.trim());
                        let found = packs::find(&dir);
                        match found.as_slice() {
                            [] => {
                                self.pack_error =
                                    Some(format!("No pack (*.omp.000) in {}", dir.display()));
                            }
                            [one] => chosen = Some(one.clone()),
                            _ => self.found = found,
                        }
                    }
                });
            });
        if let Some(p) = chosen {
            self.open(ctx, &p);
            open = false;
        }
        self.picker = open;
    }

    /// The side panel ([`side_panel`]), and what was picked in it.
    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let Some(m) = &mut self.map else { return };
        let parts = Parts {
            search: &mut m.search,
            here: &mut m.here,
            routing: &mut m.routing,
            page: &mut m.page,
            camera: &m.camera,
            pack_units: m.units,
            pins: &mut self.pins,
            trips: &mut self.trips,
            print: &mut self.print,
            exe_dir: &self.exe_dir,
        };
        let o = side_panel(ui, &mut self.cfg, parts, &mut self.bench);
        // A result picked: fly there. The bench records it, and jumps there itself.
        if let Some(b) = &mut self.bench {
            for t in &o.out.taken {
                let view = t.first.as_ref().map(|h| search::view_for(h, m.camera.size));
                b.search_answer(t, view);
            }
        } else if let Some(h) = o.out.fly {
            let (c, z) = search::view_for(&h, m.camera.size);
            m.motion.fly_to(&m.camera, c, z);
        }
        if let Some((a, h)) = o.out.route {
            m.routing.apply(a, Place::of_hit(&h));
        }
        if let Some((a, place)) = o.stop {
            m.routing.apply(a, place);
        }
        if let Some((c, z)) = o.fly {
            m.motion.fly_to(&m.camera, c, z);
        }
        // A pushpin added: from the map's menu, "what's here" or a picked result.
        if let Some((at, name)) = m.here.add_pin.take() {
            let name = name.unwrap_or_else(|| self.pins.next_name());
            self.pins.add(&name, omp_core::geo::from_unit(at));
            m.here.panel = None;
        }
        // A pushpin picked in search or on the Trips page: open it and fly there.
        if let Some(r) = o.pin.or(o.trips.show_pin) {
            self.pins.show(r);
            if let Some((c, z)) = self.pins.view_of(r, &m.camera) {
                m.motion.fly_to(&m.camera, c, z);
            }
        }
        if let Some(path) = o.trips.open {
            self.open_trip(&path);
        }
        let Some(m) = &mut self.map else { return };
        if let Some((from, to)) = o.trips.renamed
            && let Some(n) = m.routing.named.as_mut().filter(|n| n.path == from)
        {
            n.name = trips_name(&to);
            n.path = to;
        }
        if let Some(gone) = o.trips.deleted
            && m.routing.named.as_ref().is_some_and(|n| n.path == gone)
        {
            m.routing.named = None;
        }
        if let Some(ask) = o.trips.save {
            let file = m.routing.to_file(&pack_label(&m.opened));
            if let Some(n) = self.trips.save(ask, file, m.routing.named.as_ref()) {
                m.routing.named = Some(n);
            }
        }
        let ctx = ui.ctx().clone();
        self.act(&ctx, o.action);
    }

    /// Opens the trip in `path` in the route page.
    fn open_trip(&mut self, path: &Path) {
        let Some(m) = &mut self.map else { return };
        match TripFile::read(path) {
            Ok(file) => {
                let named = Named {
                    name: trips_name(path),
                    path: path.to_path_buf(),
                    file: file.clone(),
                };
                m.routing.open_file(&file, named);
                self.trips.error = None;
            }
            Err(e) => self.trips.error = Some(e),
        }
    }

    /// The save window and the file browser, for trips and pushpin sets.
    fn trip_windows(&mut self, ctx: &egui::Context) {
        if let Some(n) = self.trips.save_window(ctx, &self.exe_dir)
            && let Some(m) = &mut self.map
        {
            m.routing.named = Some(n);
        }
        match self.trips.browser_window(ctx) {
            Some(Browsed::Saved(n)) => {
                if let Some(m) = &mut self.map {
                    m.routing.named = Some(*n);
                }
            }
            Some(Browsed::OpenTrip(path)) => self.open_trip(&path),
            Some(Browsed::SavePins(set, path)) => self.pins.save_as(set, path),
            Some(Browsed::OpenPins(path)) => self.pins.open_file(path),
            None => {}
        }
    }

    fn act(&mut self, ctx: &egui::Context, action: Option<Action>) {
        match action {
            Some(Action::Pick) => self.picker = true,
            Some(Action::Verify) => self.start_verify(ctx),
            Some(Action::About) => self.about = true,
            Some(Action::Print) => {
                if let Some(m) = &mut self.map {
                    self.cfg.side_panel.open = true;
                    let mut parts = Parts {
                        search: &mut m.search,
                        here: &mut m.here,
                        routing: &mut m.routing,
                        page: &mut m.page,
                        camera: &m.camera,
                        pack_units: m.units,
                        pins: &mut self.pins,
                        trips: &mut self.trips,
                        print: &mut self.print,
                        exe_dir: &self.exe_dir,
                    };
                    open_print(ctx, &mut parts, &self.cfg.print, printui::Kind::Map);
                }
            }
            None => {}
        }
    }

    /// The buttons and readouts over the map: the button that opens the side panel at the
    /// top left when it is closed; the scale bar and the cursor's coordinates at the bottom
    /// left; the zoom buttons and the credit line at the bottom right.
    fn map_controls(&mut self, ctx: &egui::Context, map_rect: Rect) {
        let Some(m) = &mut self.map else { return };
        let units = self.cfg.units.unwrap_or(m.units);
        let panel = egui::Frame::popup(&ctx.global_style())
            .shadow(crate::theme::MAP_SHADOW)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .fill(Color32::from_white_alpha(215));
        let mut action = None;
        if !self.cfg.side_panel.open {
            let side = &mut self.cfg.side_panel;
            egui::Area::new(egui::Id::unique("side-open"))
                .fixed_pos(map_rect.left_top() + Vec2::new(8.0, 8.0))
                .show(ctx, |ui| {
                    let open = glyph_button(ui, Glyph::Show, 30.0)
                        .on_hover_text("Show the panel: search, route, layers  (Ctrl+B)");
                    if open.clicked() {
                        side.open = true;
                    }
                });
        }

        // Bottom left: scale bar and coordinates.
        let mpp = scale::metres_per_point(m.camera.lonlat()[1], m.camera.scale());
        let cursor = self.cursor;
        egui::Area::new(egui::Id::unique("map-scale"))
            .fixed_pos(map_rect.left_bottom() + Vec2::new(8.0, -8.0))
            .pivot(Align2::LEFT_BOTTOM)
            .interactable(false)
            .show(ctx, |ui| {
                panel.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    scale_bar(ui, mpp, units);
                    let text = cursor.map_or_else(String::new, |[lon, lat]| lonlat_text(lon, lat));
                    ui.add_space(3.0);
                    ui.label(egui::RichText::new(text).size(11.0).color(INK));
                });
            });

        // Bottom right: zoom buttons above the credit line. Only OpenStreetMap's credit stays
        // on the map, as its licence asks; the address sources' credits, hundreds for a large
        // pack, are in About this map, which the line opens.
        let credit = "© OpenStreetMap contributors";
        let credit_rect = egui::Area::new(egui::Id::unique("map-credit"))
            .fixed_pos(map_rect.right_bottom())
            .pivot(Align2::RIGHT_BOTTOM)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_white_alpha(190))
                    .inner_margin(egui::Margin::symmetric(4, 1))
                    .show(ui, |ui| {
                        let label =
                            egui::Label::new(egui::RichText::new(credit).size(10.5).color(INK))
                                .sense(Sense::click());
                        if ui.add(label).on_hover_text("About this map").clicked() {
                            action = Some(Action::About);
                        }
                    });
            })
            .response
            .rect;
        let center = [m.camera.size[0] / 2.0, m.camera.size[1] / 2.0];
        egui::Area::new(egui::Id::unique("map-zoom"))
            .fixed_pos(Pos2::new(map_rect.right() - 8.0, credit_rect.top() - 8.0))
            .pivot(Align2::RIGHT_BOTTOM)
            .show(ctx, |ui| {
                panel.show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // The signs are drawn, not typed: a font's + and − sit off centre.
                    let button = |ui: &mut egui::Ui, plus: bool, tip: &str| {
                        let b = egui::Button::new("").min_size(Vec2::splat(32.0));
                        let r = ui.add(b).on_hover_text(tip);
                        let (c, h) = (r.rect.center(), 6.5);
                        let stroke = Stroke::new(2.0, ui.style().interact(&r).fg_stroke.color);
                        let p = ui.painter();
                        p.line_segment([c - Vec2::new(h, 0.0), c + Vec2::new(h, 0.0)], stroke);
                        if plus {
                            p.line_segment([c - Vec2::new(0.0, h), c + Vec2::new(0.0, h)], stroke);
                        }
                        r.clicked()
                    };
                    if button(ui, true, "Zoom in  (+)") {
                        m.motion.zoom(1.0, center);
                    }
                    if button(ui, false, "Zoom out  (\u{2212})") {
                        m.motion.zoom(-1.0, center);
                    }
                });
            });

        self.act(ctx, action);
    }

    fn start_verify(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.map else { return };
        let ctx = ctx.clone();
        self.verify = Some(Verify::start(
            m.opened.reader.clone(),
            omp_core::tiles::TileReader::pack,
            move || ctx.request_repaint(),
        ));
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let mut open = true;
        egui::Window::new("About this map")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                let Some(m) = &self.map else {
                    ui.label("No pack is open.");
                    return;
                };
                let pack = m.opened.reader.pack();
                let info = &pack.info;
                ui.label(egui::RichText::new(&info.name).heading().size(18.0));
                egui::Grid::new("about-grid")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            ui.label(egui::RichText::new(k).strong());
                            ui.label(v);
                            ui.end_row();
                        };
                        row("Built", packs::date(info.build_time));
                        row("Data as of", info.snapshot.clone());
                        if let Ok(meta) = &m.opened.meta {
                            for (k, v) in packs::source_rows(&meta.sources) {
                                row(&k, v);
                            }
                            row(
                                "Builder",
                                format!("{}, format {}", meta.builder_version, meta.format_version),
                            );
                        }
                        let dir = packs::pack_dir(&m.opened.first_chunk);
                        let chunks = match pack.chunks() {
                            1 => "1 file".to_string(),
                            n => format!("{n} files"),
                        };
                        row("Pack", format!("{} ({chunks})", dir.display()));
                    });
                ui.separator();
                match &m.opened.meta {
                    Ok(meta) => {
                        // A pack with addresses credits hundreds of sources.
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .show(ui, |ui| {
                                ui.label(&meta.attribution);
                                // Sources' own attributions that the pack's does not already
                                // carry.
                                let mut shown: Vec<&str> = vec![&meta.attribution];
                                for src in
                                    meta.sources.iter().filter_map(|s| s.attribution.as_deref())
                                {
                                    if !shown.iter().any(|s| s.contains(src)) {
                                        ui.label(src);
                                        shown.push(src);
                                    }
                                }
                            });
                    }
                    Err(e) => {
                        ui.label(format!("© OpenStreetMap contributors. ({e})"));
                    }
                }
                ui.separator();
                ui.label(format!("Offline Road Maps {}", env!("CARGO_PKG_VERSION")));
            });
        self.about = open;
    }

    fn verify_window(&mut self, ctx: &egui::Context) {
        let Some(v) = &mut self.verify else { return };
        let outcome = v.outcome();
        let (done, total) = v.progress();
        let secs = v.elapsed().as_secs_f64();
        let mut open = true;
        let mut close = false;
        egui::Window::new("Verify pack")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(380.0);
                match &outcome {
                    None => {
                        ui.label("Checking every part of the pack against its checksum…");
                        let f = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        };
                        ui.add(egui::ProgressBar::new(f).show_percentage());
                        ui.label(format!(
                            "{:.0} of {:.0} MiB, {}",
                            mib(done),
                            mib(total),
                            seconds(secs)
                        ));
                        close = ui.button("Cancel").clicked();
                    }
                    Some(o) => {
                        match o {
                            Outcome::Intact { sections, bytes } => ui.label(format!(
                                "The pack is intact: all {sections} sections ({:.0} MiB) \
                                 match their checksums. Checked in {}.",
                                mib(*bytes),
                                seconds(secs)
                            )),
                            Outcome::Damaged(ids) => ui.colored_label(
                                ERROR,
                                format!(
                                    "The pack is damaged: {} {} not match {} checksum. \
                                     Copy the pack again from where it came from, or \
                                     build it again.",
                                    ids.join(", "),
                                    if ids.len() == 1 { "does" } else { "do" },
                                    if ids.len() == 1 { "its" } else { "their" },
                                ),
                            ),
                            Outcome::Failed(e) => {
                                ui.colored_label(ERROR, format!("The pack could not be read: {e}"))
                            }
                            Outcome::Cancelled => ui.label("Cancelled."),
                        };
                        close = ui.button("Close").clicked();
                    }
                }
            });
        if !open || close {
            // Dropping it stops the check.
            self.verify = None;
        }
    }

    fn error_windows(&mut self, ctx: &egui::Context) {
        if let Some(e) = &self.pack_error {
            let mut close = false;
            let mut pick = false;
            egui::Window::new("Could not open the map")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_max_width(480.0);
                    ui.colored_label(ERROR, e);
                    ui.label(
                        "The pack may be incomplete (a chunk file missing), damaged, or made \
                         for a newer version of the app.",
                    );
                    ui.horizontal(|ui| {
                        pick = ui.button("Choose a pack…").clicked();
                        close = ui.button("OK").clicked();
                    });
                });
            if pick {
                self.picker = true;
            }
            if pick || close {
                self.pack_error = None;
            }
        }
        if let Some(e) = &self.tile_error {
            let mut close = false;
            let mut verify = false;
            egui::Window::new("Part of the map could not be read")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_max_width(480.0);
                    ui.colored_label(ERROR, e);
                    ui.label(
                        "The pack may be damaged. Verify it to find out; it takes a minute or \
                         more for a large pack.",
                    );
                    ui.horizontal(|ui| {
                        verify = ui.button("Verify pack").clicked();
                        close = ui.button("Not now").clicked();
                    });
                });
            if verify {
                self.start_verify(ctx);
            }
            if verify || close {
                self.tile_error = None;
            }
        }
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

/// The search marker's result, when `at` (world) is on its pin: a route item picked there
/// is for the result, with its street.
fn marker_at<'a>(
    search: &'a SearchUi,
    cam: &Camera,
    at: [f64; 2],
) -> Option<&'a omp_core::find::Hit> {
    let h = search.session.marker.as_ref()?;
    let tip = cam.world_to_screen(omp_core::geo::to_unit(h.p));
    let p = cam.world_to_screen(at);
    let (dx, dy) = (p[0] - tip[0], p[1] - tip[1]);
    // The pin: its tip, and its head 22 points above it.
    let on = dx.hypot(dy) < 8.0
        || dx.hypot(dy + 22.0) < 11.0
        || (dx.abs() < 6.0 && (-22.0..0.0).contains(&dy));
    on.then_some(h)
}

/// "Ohio, built 2026-10-28": the pack a trip is saved on, for its file.
fn pack_label(opened: &Opened) -> String {
    let info = &opened.reader.pack().info;
    format!("{}, built {}", info.name, packs::date(info.build_time))
}

fn trips_name(path: &Path) -> String {
    crate::files::stem(path)
}

/// The parts of the open map that the side panel shows, and the app's trips and pushpins.
struct Parts<'a> {
    search: &'a mut SearchUi,
    here: &'a mut HereUi,
    routing: &'a mut RouteUi,
    page: &'a mut Page,
    camera: &'a Camera,
    /// The units the pack suggests.
    pack_units: Units,
    pins: &'a mut PinsUi,
    trips: &'a mut TripsUi,
    print: &'a mut PrintUi,
    exe_dir: &'a Path,
}

/// What was picked in the side panel, done once it is drawn.
struct SideOut {
    out: search::Out,
    /// A route button of "what's here" or the picked result.
    stop: Option<(StopAction, Place)>,
    /// A step of the directions: its view.
    fly: Option<(crate::camera::Vec2, f64)>,
    action: Option<Action>,
    /// A pushpin found by search, clicked.
    pin: Option<PinRef>,
    /// What the Trips page asked for.
    trips: trips::PageOut,
    /// Where the tabs and the button that hides the panel were drawn, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    tabs: [Rect; 5],
    #[cfg_attr(not(test), allow(dead_code))]
    hide: Rect,
}

/// The bench's stops, set as the right-click menu would: the first as the start, the last
/// as the destination. Replaces any stops there were.
fn set_stops(routing: &mut RouteUi, stops: &[crate::camera::Vec2]) {
    if routing.has_stops() {
        routing.apply(StopAction::Clear, Place::at([0.0, 0.0]));
    }
    let last = stops.len().saturating_sub(1);
    for (i, &w) in stops.iter().enumerate() {
        let a = match i {
            0 => StopAction::From,
            i if i == last => StopAction::To,
            _ => StopAction::Add,
        };
        routing.apply(a, Place::at(w));
    }
}

/// The side panel, on the left when open: the search box with the ☰ menu, the tabs, and the
/// page. The keys and the answers of its parts are handled whether it is open or not: Ctrl+B
/// opens and closes it, Ctrl+F and "/" open it on the search box, "what's here" opens it on
/// the search page and a stop set on the route page, and Escape steps back in the page once
/// the search box has let go of it.
fn side_panel(
    ui: &mut egui::Ui,
    cfg: &mut Config,
    mut p: Parts<'_>,
    bench: &mut Option<Bench>,
) -> SideOut {
    let ctx = ui.ctx().clone();
    let mut o = SideOut {
        out: search::Out::default(),
        stop: None,
        fly: None,
        action: None,
        pin: None,
        trips: trips::PageOut::default(),
        tabs: [Rect::NOTHING; 5],
        hide: Rect::NOTHING,
    };
    let units = cfg.units.unwrap_or(p.pack_units);
    let recent = &mut cfg.recent_searches;
    let side = &mut cfg.side_panel;
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)) {
        side.open = !side.open;
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)) {
        side.open = true;
        open_print(&ctx, &mut p, &cfg.print, printui::Kind::Map);
    }
    o.out = p.search.poll(recent);
    if p.search.shortcut(&ctx) {
        side.open = true;
        *p.page = Page::Search;
    }
    if !p.search.box_was_focused() {
        match p.page {
            Page::Search => {
                let _ = p.pins.escape(&ctx) || p.here.escape(&ctx) || p.search.escape(&ctx);
            }
            Page::Route => {
                p.routing.escape(&ctx);
            }
            Page::Trips | Page::Layers => {}
            Page::Print => {
                if p.print.escape(&ctx) {
                    *p.page = p.print.back;
                }
            }
        }
    }
    // The bench asks "what's here" at the view's centre, as the right-click menu would, and
    // types into the box as a user would.
    let available = matches!(p.search.status(), Status::Opening | Status::Ready);
    if bench.as_mut().is_some_and(|b| b.here_ask(available)) {
        p.here.ask(p.search, p.camera.center, p.camera);
    }
    // A result picked opens "what's here" at it, with the route buttons. Not in the bench,
    // so its searches are timed as before.
    if bench.is_none() && p.here.follow(p.search, p.camera) {
        ctx.request_repaint();
    }
    for t in p.here.take(p.search) {
        if let Some(b) = bench.as_mut() {
            b.here_answer(&t);
        }
    }
    for (at, address) in p.here.take_stops() {
        p.routing.name_stops_at(at, &address);
    }
    let key = bench.as_mut().and_then(|b| b.search_key(available));
    if std::mem::take(&mut p.here.asked) || key.is_some() {
        side.open = true;
        *p.page = Page::Search;
    }
    if std::mem::take(&mut p.routing.opened) {
        side.open = true;
        *p.page = Page::Route;
    }
    if std::mem::take(&mut p.pins.opened) {
        side.open = true;
        *p.page = Page::Search;
    }
    if let Some(page) = bench.as_mut().and_then(Bench::page_shot) {
        *p.page = page;
        p.search.hide(&ctx);
    }
    p.search.route_menu = p.routing.menu_state();
    if !side.open {
        p.search.hide(&ctx);
        return o;
    }

    let range = side::width_range(ctx.content_rect().width());
    // The config holds the width, not egui: a window made narrow for a while does not keep
    // the panel narrow.
    let id = ui.scope_id().with("side-panel");
    ui.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(id));
    let asked = side.width.clamp(range.min, range.max);
    let frame = egui::Frame::side_top_panel(ui.style()).inner_margin(8);
    let (debug, layers, units_cfg) = (&mut cfg.debug_overlay, &mut cfg.layers, &mut cfg.units);
    let schedule = &mut cfg.schedule;
    let print_cfg = &mut cfg.print;
    let r = egui::Panel::left("side-panel")
        .resizable(true)
        .default_size(asked)
        .size_range(range)
        .frame(frame)
        .show(ui, |ui| {
            let had_focus = p.search.box_was_focused();
            // From the right: hide, the menu, and the box in what is left.
            let row = egui::Layout::right_to_left(egui::Align::Center);
            let size = Vec2::new(ui.available_width(), crate::theme::HEADER_H);
            ui.allocate_ui_with_layout(size, row, |ui| {
                let hide = glyph_button(ui, Glyph::Hide, crate::theme::HEADER_H)
                    .on_hover_text("Hide the panel  (Ctrl+B)");
                o.hide = hide.rect;
                if hide.clicked() {
                    side.open = false;
                }
                o.action = side::menu(ui, debug);
                p.search.box_ui(ui, p.camera, recent, key, &mut o.out);
            });
            // A click in the box shows the search page.
            if p.search.focused(&ctx) && !had_focus {
                *p.page = Page::Search;
            }
            // More room above the tabs than below them.
            ui.add_space(5.0);
            let before = *p.page;
            o.tabs = side::tabs(ui, p.page);
            // The Print tab opens the print page as the menu does.
            if *p.page == Page::Print && before != Page::Print {
                *p.page = before;
                open_print(&ctx, &mut p, print_cfg, printui::Kind::Map);
            }
            ui.add(egui::Separator::default().spacing(2.0));
            ui.add_space(3.0);
            match p.page {
                Page::Search => {
                    o.pin = p.pins.found_ui(ui, &p.search.text);
                    if !p
                        .search
                        .suggestions_ui(ui, p.camera, units, recent, &mut o.out)
                    {
                        o.stop = search_page(ui, &mut p, units, &mut o.out);
                    }
                }
                Page::Route => {
                    o.fly = p.routing.page_ui(ui, p.camera, units);
                    // The settings last used are the next trip's.
                    if *schedule != p.routing.plan.s {
                        *schedule = p.routing.plan.s.clone();
                    }
                }
                Page::Trips => {
                    let can_open = p.routing.status().usable();
                    let current = trips::Current {
                        can_save: p.routing.from.is_some() && p.routing.to.is_some(),
                        named: p.routing.named.as_ref(),
                        unsaved: p.routing.unsaved(),
                    };
                    o.trips = p.trips.page_ui(ui, p.pins, current, can_open, p.exe_dir);
                }
                Page::Layers => side::layers_ui(ui, layers, units_cfg, p.pack_units),
                Page::Print => {
                    let around = printui::Around {
                        cam: p.camera,
                        units,
                        has_route: p.routing.route.is_some(),
                        route_bounds: p.routing.route_bounds(),
                    };
                    p.print.page_ui(ui, print_cfg, &around);
                }
            }
        });
    let w = r.response.rect.width();
    if (w - asked).abs() > 0.5 {
        side.width = w.round();
    }
    o
}

/// Opens the print page on `kind`, to go back to the page it was opened from.
fn open_print(
    ctx: &egui::Context,
    p: &mut Parts<'_>,
    settings: &crate::print::Settings,
    kind: printui::Kind,
) {
    if *p.page != Page::Print {
        p.print.back = *p.page;
    }
    *p.page = Page::Print;
    let around = printui::Around {
        cam: p.camera,
        units: Units::Miles,
        has_route: p.routing.route.is_some(),
        route_bounds: p.routing.route_bounds(),
    };
    p.print.open(ctx, kind, &around, settings);
}

/// Moves the camera with the map's left edge, which the side panel moves when it opens,
/// closes or is resized, so what is on the map stays where it was on screen. `last` is the
/// map's rectangle the frame before.
fn keep_still(cam: &mut Camera, last: Rect, rect: Rect) {
    if last.is_positive() && last.left() != rect.left() {
        let dx = rect.center().x - last.center().x;
        cam.center[0] += f64::from(dx) / cam.scale();
    }
}

/// The search page under the suggestions: "what's here" for a right-clicked point or the
/// picked result, then the results of Enter; a few words on what to do when there is
/// neither. Returns a route button clicked, with its stop.
fn search_page(
    ui: &mut egui::Ui,
    p: &mut Parts<'_>,
    units: Units,
    out: &mut search::Out,
) -> Option<(StopAction, Place)> {
    let menu = p.routing.menu_state();
    let pin_stop = p.pins.panel_ui(ui, &menu);
    if p.pins.open.is_some() && p.here.panel.is_some() {
        ui.separator();
    }
    let here = p.here.panel.is_some() || p.pins.open.is_some();
    let stop = p
        .here
        .page_ui(ui, units, p.search.icons(), &menu)
        .or(pin_stop);
    if !p.search.has_results() {
        if !here {
            let weak = |t: &str| egui::RichText::new(t).color(crate::theme::WEAK);
            ui.label(weak("Type a place, an address, a postcode or a business."));
            ui.add_space(4.0);
            ui.label(weak(
                "Right-click the map for what is there, to route from or to a point, or to add a pushpin.",
            ));
        }
        return stop;
    }
    if here {
        ui.separator();
    }
    p.search.results_ui(ui, p.camera, units, out);
    stop
}

const ERROR: Color32 = Color32::from_rgb(180, 30, 30);

/// One bar per unit, at most 120 points long: miles with its ticks up, kilometres below it
/// with its ticks down.
fn scale_bar(ui: &mut egui::Ui, metres_per_point: f64, units: Units) {
    const MAX: f64 = 120.0;
    let bars: &[(bool, bool)] = match units {
        Units::Miles => &[(true, true)],
        Units::Kilometres => &[(false, true)],
        Units::Both => &[(true, true), (false, false)],
    };
    let stroke = Stroke::new(1.5, INK);
    let font = FontId::proportional(11.0);
    for &(imperial, up) in bars {
        let (len, text) = scale::bar(metres_per_point, MAX, imperial);
        let label = ui
            .painter()
            .layout_no_wrap(text.to_string(), font.clone(), INK);
        let width = 1.0 + len as f32 + 5.0 + label.size().x;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 13.0), Sense::hover());
        let p = ui.painter();
        let (x0, x1) = (rect.left() + 1.0, rect.left() + 1.0 + len as f32);
        let (y, tip) = match up {
            true => (rect.bottom() - 0.75, rect.top() + 3.0),
            false => (rect.top() + 0.75, rect.bottom() - 3.0),
        };
        p.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], stroke);
        p.line_segment([Pos2::new(x0, y), Pos2::new(x0, tip)], stroke);
        p.line_segment([Pos2::new(x1, y), Pos2::new(x1, tip)], stroke);
        p.text(
            Pos2::new(x1 + 5.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            INK,
        );
    }
}

fn seconds(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.1} s")
    } else {
        format!("{s:.0} s")
    }
}

fn mib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
#[path = "side_tests.rs"]
mod side_tests;
