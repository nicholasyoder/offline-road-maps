//! The window: the frame loop, the map with its input, and the debug overlay (F3, with tile
//! outlines). Its parts are in their own modules: the side panel on the left (search,
//! "what's here", the route, trips, layers and the ☰ menu; phase 3 step 3.8) in [`panel`],
//! the controls over the map (scale bar, cursor coordinates, zoom buttons, credit line, and
//! a button to open the panel when it is closed) in [`controls`], the dialogs (pack picker,
//! About this map, Verify pack, errors and notices) in [`dialogs`], opening a pack in
//! [`open`], and what the bench asks of the frame in [`bench`]. Saved trips and pushpins live
//! next to the executable and outlast the pack: they belong to the app, not to the open map.

mod bench;
mod controls;
mod dialogs;
mod open;
mod panel;
mod settings;

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use eframe::egui_wgpu::{self, wgpu};

use crate::bench::Bench;
use crate::cache::Cache;
use crate::camera::{Camera, Motion};
use crate::config::Movement;
use crate::config::{Config, Location, View};
use crate::here::HereUi;
use crate::keys::{self, Command};
use crate::labels::{self, Placer, TileLabels};
use crate::layers::{Hidden, Layers};
use crate::loader::{Done, Loader};
use crate::memory::mib;
use crate::mesh::{Prepared, TileInfo};
use crate::packs::Opened;
use crate::pins::{self, GpuPins, PinsUi};
use crate::pool::Pool;
use crate::printui::{self, PrintUi};
use crate::render::{self, Frame, GpuMesh, MapCallback, Renderer};
use crate::routing::{Bands, Place, RouteDraw, RouteUi, StopAction};
use crate::scale::Units;
use crate::search::{self, SearchUi};
use crate::side::Page;
use crate::style::{self, Style, StyleFile};
use crate::styles::{self, Entry};
use crate::tiles::{self, Cover, Known, Resolved, TileId};
use crate::trips::TripsUi;
use crate::verify::Verify;

use bench::set_stops;
use open::{compile_style, start_loader};

/// Tile cache budget: the tiles' vertex data, which lives on the GPU (phase1-plan.md 1.4).
const CACHE_BUDGET: usize = 96 << 20;
/// Shown where no tile is drawn: outside the pack, and tiles not loaded yet.
pub const NO_MAP: Color32 = Color32::from_rgb(222, 222, 216);
/// How often a changed view is written to `config.toml`.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// Tile timings kept for the overlay's averages.
const RECENT_TILES: usize = 256;

/// What the app was started with.
pub struct Startup {
    pub exe_dir: PathBuf,
    pub config: Location,
    /// What it holds, loaded once (a damaged file is renamed as it is read), and what to
    /// tell the user about it.
    pub cfg: Config,
    pub cfg_note: Option<String>,
    /// The first-chunk file to open.
    pub chosen: Option<PathBuf>,
    pub notices: Vec<String>,
    pub bench: Option<Bench>,
    pub started: Instant,
    /// MSAA samples the window was created with.
    pub msaa: u32,
    /// `--style`: the style file that is used whatever the config says.
    pub style_arg: Option<PathBuf>,
}

enum TileData {
    /// Not in the pack; drawn from its nearest stored ancestor.
    Absent,
    Mesh(Arc<GpuMesh>, Arc<TileLabels>, TileInfo),
    Error,
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
    /// The last frame of the map and what it was made from. It is drawn again while none of
    /// that changes, so a still map places no labels and writes nothing to the GPU (step
    /// 5.2, P11–P13).
    last_frame: Option<(FrameKey, Arc<Frame>)>,
}

/// What a frame of the map is made from, besides the time, which only labels fading in
/// need, and the style, whose reload starts over.
struct FrameKey {
    camera: Camera,
    pixels_per_point: f32,
    srgb: bool,
    layers: Layers,
    /// The factor on the labels' sizes (Settings).
    label_size: f64,
    /// [`Cache::version`]: what the frame can draw of the tiles.
    tiles: u64,
    /// The buffers of the route's lines and of the pins; the last frame holds them, so a
    /// new one is never at the address of an old one.
    route: Bands,
    pins: Option<Arc<GpuPins>>,
}

impl FrameKey {
    fn same(&self, o: &Self) -> bool {
        fn same<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (a, b) => a.is_none() && b.is_none(),
            }
        }
        self.camera == o.camera
            && self.pixels_per_point == o.pixels_per_point
            && self.srgb == o.srgb
            && self.layers == o.layers
            && self.label_size == o.label_size
            && self.tiles == o.tiles
            && self.route.iter().zip(&o.route).all(|(a, b)| same(a, b))
            && same(&self.pins, &o.pins)
    }
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
    /// The file browser that chooses a map, while it is open.
    picker: Option<crate::files::Browser>,
    map: Option<MapState>,
    notices: Vec<String>,
    bench: Option<Bench>,
    started: Instant,
    first_full: Option<Duration>,
    last_frame: Option<Instant>,
    /// The UI thread's allocations when the last frame started (`count-allocs` builds).
    allocs: Option<(u64, u64)>,
    /// Recent frames: (start, interval ms, cpu ms).
    frames: VecDeque<(Instant, f32, f32)>,
    /// The config as last saved.
    saved: Config,
    last_save: Instant,
    cursor: Option<[f64; 2]>,
    adapter: String,
    device: Option<wgpu::Device>,
    /// The window's adapter, device and queue, shared with the print page's workers.
    gpu: Option<crate::snapshot::Shared>,
    /// Tile geometry on the GPU.
    pool: Option<Arc<Pool>>,
    srgb: bool,
    /// The styles the Map tab lists, and the one in use: its id, and the file it was
    /// compiled from.
    styles: Vec<Entry>,
    style_id: String,
    style: StyleFile,
    /// The file of the style in use, when it is not built in.
    style_path: Option<PathBuf>,
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
    /// The Keyboard shortcuts window is open.
    shortcuts: bool,
    /// The window has been checked against its monitor once.
    window_checked: bool,
    /// The Settings window, when open.
    settings: Option<settings::Window>,
    /// egui's zoom factor as last seen, to tell a change of its own from ours.
    zoom_seen: f32,
    /// The system's light or dark, for the UI theme setting "as the system".
    sys_theme: crate::systheme::Watch,
    /// MSAA samples the window was created with.
    msaa_now: u32,
    /// Saved trips and pushpins (phase 4 step 4.7).
    trips: TripsUi,
    pins: PinsUi,
    /// The print page and its preview (phase 4 step 4.9).
    print: PrintUi,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, s: Startup) -> Self {
        let cfg = s.cfg;
        let cfg_zoom = cfg.display.ui_size as f32;
        let mut notices = s.notices;
        notices.extend(s.config.fallback.clone());
        notices.extend(s.cfg_note);
        let styles = styles::list(&s.exe_dir, s.style_arg.as_deref());
        // The bench draws the default style unless it is given one.
        let want = match (&s.style_arg, &s.bench) {
            (Some(_), _) => styles::FORCED,
            (None, Some(_)) => styles::DEFAULT,
            (None, None) => cfg.style.as_str(),
        };
        let (style_id, style, why) = pick_style(&styles, want);
        notices.extend(why);
        style::set_overlay(style.overlay);
        let sys_theme = crate::systheme::Watch::start(&cc.egui_ctx);
        crate::theme::apply(
            &cc.egui_ctx,
            crate::theme::wants_dark(cfg.display.theme, sys_theme.get(&cc.egui_ctx)),
        );
        cc.egui_ctx.set_zoom_factor(cfg.display.ui_size as f32);
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
            print: PrintUi::new(&s.exe_dir),
            exe_dir: s.exe_dir,
            cfg_loc: s.config,
            saved: cfg.clone(),
            cfg,
            // Not opened by itself: a dialog window that starts behind the map's keeps the
            // map's window from being drawn (it waits for the dialog's frame), so the
            // no-map screen's button opens it instead.
            picker: None,
            map: None,
            notices,
            bench: s.bench,
            started: s.started,
            first_full: None,
            last_frame: None,
            allocs: None,
            frames: VecDeque::new(),
            last_save: Instant::now(),
            cursor: None,
            adapter,
            device: rs.map(|rs| rs.device.clone()),
            gpu: rs.map(|rs| (rs.adapter.clone(), rs.device.clone(), rs.queue.clone())),
            pool: rs.map(|rs| Pool::new(rs.device.clone(), rs.queue.clone())),
            srgb: rs.is_some_and(|rs| rs.target_format.is_srgb()),
            style_path: styles::find(&styles, &style_id).and_then(|e| e.path.clone()),
            style_mtime: None,
            style_id,
            style,
            styles,
            style_checked: Instant::now(),
            pack_error: None,
            tile_error: None,
            verify: None,
            about: false,
            shortcuts: false,
            window_checked: false,
            settings: None,
            zoom_seen: cfg_zoom,
            sys_theme,
            msaa_now: s.msaa,
        };
        if let Some(p) = s.chosen {
            app.open(&cc.egui_ctx, &p);
        }
        app
    }

    /// Debug builds: reads the style file in use again when it has changed, and redraws
    /// the map with it. Checked once a second.
    fn reload_style(&mut self, ctx: &egui::Context) {
        const EVERY: Duration = Duration::from_secs(1);
        if !cfg!(debug_assertions) || self.bench.is_some() {
            return;
        }
        let Some(path) = self.style_path.clone() else {
            return;
        };
        ctx.request_repaint_after(EVERY);
        if self.style_checked.elapsed() < EVERY {
            return;
        }
        self.style_checked = Instant::now();
        let t = mtime(&path);
        if t.is_none() || t == self.style_mtime {
            return;
        }
        if self.style_mtime.is_none() {
            // The first look: remember the file as it is.
            self.style_mtime = t;
            return;
        }
        self.style_mtime = t;
        match StyleFile::read(&path) {
            Ok(f) => {
                if let Some(e) = self.styles.iter_mut().find(|e| e.id == self.style_id) {
                    e.file = Ok(f.clone());
                }
                self.style = f;
            }
            Err(e) => {
                self.notices.push(format!("Style not reloaded: {e}"));
                return;
            }
        }
        eprintln!("style: reloaded {}", path.display());
        self.apply_style(ctx);
    }

    /// Switches to the style `id` of the Map tab: the tiles in view are tessellated again
    /// and the choice is kept in the config. A style that cannot be used is reported and
    /// leaves the one in use.
    fn set_style(&mut self, ctx: &egui::Context, id: &str) {
        if id == self.style_id {
            return;
        }
        let Some(entry) = styles::find(&self.styles, id) else {
            return;
        };
        let file = match &entry.file {
            Ok(f) => f.clone(),
            Err(e) => {
                self.notices
                    .push(format!("{} cannot be used: {e}", entry.name));
                return;
            }
        };
        self.style_path = entry.path.clone();
        self.style_mtime = None;
        self.style_id = id.into();
        self.style = file;
        // `--style` is this run's choice only.
        if id != styles::FORCED {
            self.cfg.style = id.into();
        }
        self.apply_style(ctx);
    }

    /// Compiles the style in use for the open pack and draws everything with it. The meshes
    /// hold style IDs, so every tile is prepared again.
    fn apply_style(&mut self, ctx: &egui::Context) {
        style::set_overlay(self.style.overlay);
        if let (Some(m), Some(pool)) = (&mut self.map, self.pool.clone()) {
            let (style, warned) = compile_style(&self.style, &m.opened, &self.style_id);
            m.style = style;
            self.notices.extend(warned);
            m.loader = start_loader(&m.opened, &m.style, pool, ctx);
            m.cache = Cache::new(CACHE_BUDGET);
            m.placer = Placer::default();
            m.hidden_for = None;
            m.last_frame = None;
        }
        ctx.request_repaint();
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

    /// Keeps the window's size and place in the config for the next start; the first time,
    /// brings a window that the monitor cannot hold back onto it.
    fn track_window(&mut self, ctx: &egui::Context) {
        if self.bench.is_some() {
            return;
        }
        let (inner, outer, monitor, maximized, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.outer_rect,
                v.monitor_size,
                v.maximized == Some(true),
                v.minimized == Some(true),
            )
        });
        let w = &mut self.cfg.window;
        if !self.window_checked
            && let Some(m) = monitor
        {
            self.window_checked = true;
            if let Some((size, pos)) = w.fit([m.x, m.y]).filter(|_| !maximized) {
                w.size = size;
                w.pos = Some(pos);
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.into()));
            }
            return;
        }
        if minimized {
            return;
        }
        w.maximized = maximized;
        if let Some(m) = monitor {
            w.monitor = Some([m.x, m.y]);
        }
        // A maximized window's size is the screen's: the restored size is kept.
        if !maximized && self.window_checked {
            if let Some(r) = inner {
                w.size = [r.width().round(), r.height().round()];
            }
            if let Some(r) = outer {
                w.pos = Some([r.min.x.round(), r.min.y.round()]);
            }
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

/// The style `want` names, with its id, else the default; and why not, when that is so.
fn pick_style(styles: &[Entry], want: &str) -> (String, StyleFile, Option<String>) {
    let mut why = None;
    if let Some(e) = styles::find(styles, want) {
        match &e.file {
            Ok(f) => return (e.id.clone(), f.clone(), None),
            Err(err) => why = Some(format!("{err}\nUsing the default style.")),
        }
    }
    (styles::DEFAULT.into(), StyleFile::builtin(), why)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
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

    fn input(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        origin: Pos2,
        dt: f64,
        pan: bool,
        mv: &Movement,
    ) {
        let cam = &mut self.camera;
        if resp.dragged() && pan {
            let d = resp.drag_delta();
            cam.pan([f64::from(d.x), f64::from(d.y)]);
            self.motion.stop_flying();
            self.motion.velocity = [0.0, 0.0];
        }
        if resp.drag_stopped() && pan {
            let v = ui.input(|i| i.pointer.velocity());
            if mv.inertia {
                self.motion.velocity = [f64::from(v.x), f64::from(v.y)];
            }
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
                                egui::MouseWheelUnit::Line => f64::from(delta.y) * mv.wheel_zoom,
                                // A notch is a quarter zoom (the default) in 100 points.
                                egui::MouseWheelUnit::Point => {
                                    f64::from(delta.y) / 400.0 * mv.wheel_zoom / 0.25
                                }
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
                let [left, right, up, down] = keys::ARROWS;
                let dx = key(left) - key(right);
                let dy = key(up) - key(down);
                if dx != 0.0 || dy != 0.0 {
                    cam.pan([dx * mv.key_pan * dt, dy * mv.key_pan * dt]);
                    self.motion.stop_flying();
                    self.motion.velocity = [0.0, 0.0];
                }
            }
        });
        if keys::take(ui.ctx(), Command::ZoomIn) {
            self.motion.zoom(mv.key_zoom, center);
        }
        if keys::take(ui.ctx(), Command::ZoomOut) {
            self.motion.zoom(-mv.key_zoom, center);
        }
    }

    fn keys_held(ui: &egui::Ui) -> bool {
        keys::arrow_down(ui.ctx())
    }

    /// Draws the map for the current camera, with what `layers` shows: the last frame again
    /// if nothing it was made from has changed, else a new one ([`Self::frame`]). With
    /// `outlines`, also the tile outlines and their numbers.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        ppp: f32,
        srgb: bool,
        layers: &Layers,
        label_size: f64,
        outlines: bool,
        pool: &Arc<Pool>,
        pins: &mut PinsUi,
    ) {
        if self.hidden_for.as_ref() != Some(layers) {
            self.hidden = Hidden::new(layers, &self.style, &self.opened.schema);
            self.hidden_for = Some(*layers);
        }
        let key = FrameKey {
            camera: self.camera.clone(),
            pixels_per_point: ppp,
            srgb,
            layers: *layers,
            label_size,
            tiles: self.cache.version,
            route: self.routing.bands(pool, &self.camera),
            pins: pins.gpu(pool),
        };
        // Not while labels fade in or the atlas left some out, nor with the outlines, which
        // need the tile plan.
        let again = self.last_frame.as_ref().filter(|(k, _)| {
            k.same(&key) && !self.view.fading && !self.placer.atlas.full() && !outlines
        });
        let frame = match again {
            Some((_, f)) => {
                self.view.label_time = Duration::ZERO;
                f.clone()
            }
            None => Arc::new(self.frame(painter, rect.min, &key, outlines)),
        };
        if !self.no_map {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                MapCallback(frame.clone()),
            ));
        }
        self.last_frame = Some((key, frame));
    }

    /// Picks and requests the tiles for `key`'s camera, and lays out a frame of them, the
    /// route, the labels and the pins; with `outlines`, draws the tile outlines.
    fn frame(
        &mut self,
        painter: &egui::Painter,
        origin: Pos2,
        key: &FrameKey,
        outlines: bool,
    ) -> Frame {
        let (ppp, srgb) = (key.pixels_per_point, key.srgb);
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
        frame.route = RouteDraw::layout(&key.route, cam, ppp);
        frame.pins = key.pins.clone().map(|p| p.draw(cam, ppp));
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
        self.placer.label_scale = labels::LabelScale(key.label_size as f32);
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
        self.view = view;
        if outlines {
            self.outlines(painter, origin, &plan.draws);
        }
        frame
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
        let allocs = crate::allocs::this_thread();
        let frame_allocs = allocs.zip(self.allocs).map(|(a, b)| (a.0 - b.0, a.1 - b.1));
        self.allocs = allocs;
        let dt = interval.as_secs_f64().min(0.1);
        let ctx = ui.ctx().clone();
        self.reload_style(&ctx);
        if let Some(id) = self.bench.as_mut().and_then(|b| b.style_ask.take()) {
            self.set_style(&ctx, &id);
        }
        self.sync_zoom(&ctx);
        self.sync_theme(&ctx);
        self.track_window(&ctx);

        if keys::take(&ctx, Command::DebugOverlay) {
            self.cfg.debug_overlay = !self.cfg.debug_overlay;
        }
        if keys::take(&ctx, Command::OpenMap) && self.bench.is_none() {
            match self.picker {
                Some(_) => self.picker = None,
                None => self.open_picker(),
            }
        }
        if keys::take(&ctx, Command::Shortcuts) {
            self.shortcuts = !self.shortcuts;
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
                    self.no_map(ui, rect);
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
                match &mut self.bench {
                    None => {
                        if let Some((c, z)) = fit {
                            m.motion.fly_to(&m.camera, c, z);
                        }
                    }
                    Some(b) => bench::routing(b, m, waiting, looking),
                }
                if self.bench.is_none() {
                    // The print page's rectangle takes a drag that starts on it.
                    let held = m.page == Page::Print
                        && self.print.shows_rectangle()
                        && self
                            .print
                            .pointer(&ctx, &resp, rect.min, &m.camera, &self.cfg.print);
                    take_focus(ui, &resp);
                    m.input(ui, &resp, rect.min, dt, !held, &self.cfg.movement);
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
                                // A click elsewhere on the map drops the search's marker.
                                let world = m.camera.screen_to_world(at);
                                if marker_at(&m.search, &m.camera, world).is_none() {
                                    m.search.session.marker = None;
                                }
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
                    m.motion.step(&mut m.camera, dt, &self.cfg.movement);
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
                        self.cfg.display.label_size,
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

        self.bench_open_print(&ctx);
        let mut close_print = false;
        if let Some(m) = &self.map
            && m.page == Page::Print
        {
            let inputs = printui::Inputs {
                pack: &m.opened.first_chunk,
                style: styles::for_print(&self.styles, &self.cfg.print.style, &self.style),
                layers: self.cfg.layers,
                units: self.cfg.units.unwrap_or(m.units),
                msaa: render::samples(self.cfg.msaa),
                settings: self.cfg.print.clone(),
                routing: &m.routing,
                pins: &self.pins,
                gpu: self.gpu.clone(),
            };
            self.print.tick(&ctx, &inputs);
            self.print.big_window(&ctx);
            if let Some(b) = &mut self.bench {
                close_print = bench::print(b, &mut self.print, &ctx, &inputs);
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
        self.shortcuts_window(&ctx);
        self.settings_window(&ctx);
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

        if self.bench_frame(&ctx, interval, cpu, frame_allocs) {
            animate = true;
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
        printui::EXITING.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// Using the map (a click, a drag, the wheel or a pinch) takes the focus from a text field,
/// whose cursor would otherwise blink on, waking the app twice a second (step 5.2, P15).
fn take_focus(ui: &egui::Ui, resp: &egui::Response) {
    let wheel = || {
        ui.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::MouseWheel { .. } | egui::Event::Zoom(_)))
        })
    };
    if resp.clicked() || resp.drag_started() || (resp.hovered() && wheel()) {
        ui.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A focused field lets go when the wheel turns over the map, and not before.
    #[test]
    fn the_map_takes_the_focus() {
        let ctx = egui::Context::default();
        let field = egui::Id::unique("field");
        let mut text = String::from("Columbus");
        let mut frame = |events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                focused: true,
                ..Default::default()
            };
            let mut full = ctx.run_ui(raw, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(field));
                let map = Rect::from_min_size(Pos2::new(0.0, 100.0), Vec2::new(800.0, 500.0));
                let resp = ui.allocate_rect(map, Sense::click_and_drag());
                take_focus(ui, &resp);
            });
            full.textures_delta.clear();
            ctx.memory(|m| m.has_focus(field))
        };
        ctx.memory_mut(|m| m.request_focus(field));
        assert!(frame(vec![egui::Event::PointerMoved(Pos2::new(
            400.0, 300.0
        ))]));
        assert!(frame(Vec::new()));
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Wheel,
        };
        assert!(!frame(vec![wheel]));
    }
}
