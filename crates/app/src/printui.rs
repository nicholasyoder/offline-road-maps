//! The print page of the side panel and its preview (phase 4 step 4.9): the label,
//! paper, orientation, text size and copies; the rectangle on the map that is the page's
//! ground, which can be moved and resized; and a preview of the page, drawn by a worker thread
//! with [`crate::pages`] as the printer will get it, only less sharp.
//!
//! The Content setting chooses the map or the directions; the directions have no rectangle.
//! *Print* sends the pages to the chosen printer and *Save as PDF* writes them to a file (step
//! 4.10), both from a second worker that draws them at the printer's resolution, one page at a time.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Pos2, Rect, Response, Sense, Stroke, TextureHandle, Vec2,
};

use crate::camera::Camera;
use crate::files::{self, Browser, Mode, Outcome};
use crate::layers::Layers;
use crate::packs;
use crate::pages::{self, Egui, MapPage, Page as Printed};
use crate::pdf::Pdf;
use crate::pins::PinsUi;
use crate::print::{Area, Layout, Orientation, Paper, Settings, TextSize};
use crate::printer::{self, Capabilities, Printer};
use crate::routing::{DirectionsDoc, RouteUi};
use crate::scale::Units;
use crate::side::Page;
use crate::snapshot::Gpu;
use crate::style::{Style, StyleFile};
use crate::theme::{ACCENT, INK, WEAK};

/// The resolution of the previews: the page as it prints, only less sharp.
const PREVIEW_DPI: f64 = 110.0;
/// How long the page's settings must hold still before the preview is drawn again.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// How near a point a corner's handle takes a click, in points.
const GRAB: f32 = 11.0;

/// The most dpi a page is printed at when the window's GPU is GL.
const GL_MAX_DPI: f64 = 360.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Map,
    Directions,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Map => "Map",
            Kind::Directions => "Directions",
        }
    }
}

/// What the preview is made from, which the app gathers each frame the page is open.
pub struct Inputs<'a> {
    pub pack: &'a Path,
    pub style: &'a StyleFile,
    pub layers: Layers,
    pub units: Units,
    pub msaa: u32,
    pub settings: Settings,
    pub routing: &'a RouteUi,
    pub pins: &'a PinsUi,
    /// The window's GPU, for the workers; they open their own without it.
    pub gpu: Option<crate::snapshot::Shared>,
}

/// What the side panel tells the page about the map.
pub struct Around<'a> {
    pub cam: &'a Camera,
    pub units: Units,
    pub has_route: bool,
    pub route_bounds: Option<[f64; 4]>,
}

enum Drag {
    /// The rectangle moves; the pointer holds it at this offset from its centre.
    Move([f64; 2]),
    /// A corner is resized; the opposite one stays at this world point.
    Corner([f64; 2], bool),
}

/// The page drawn, as textures.
#[derive(Default)]
struct Preview {
    signature: u64,
    pages: Vec<TextureHandle>,
    error: Option<String>,
    took: Duration,
    note: String,
}

pub struct PrintUi {
    pub kind: Kind,
    label: String,
    area: Option<Area>,
    /// The first area's shape, which *Automatic* orientation follows.
    wide: bool,
    drag: Option<Drag>,
    worker: Option<Worker>,
    /// The page to go back to when this one closes.
    pub back: Page,
    seq: u64,
    /// The signature of what was last seen, and since when.
    seen: Option<(u64, Instant)>,
    /// What the worker was last asked, and for which request.
    asked: Option<(u64, u64)>,
    preview: Preview,
    /// The large preview, open at this page.
    big: Option<usize>,
    /// The printers, read when the page opens.
    printers: Vec<Printer>,
    /// The printers being asked for, off the UI thread (the system can be slow to answer).
    listing: Option<Receiver<(Option<String>, Vec<Printer>)>>,
    /// What the chosen printer can do, and for which printer.
    caps: Option<(String, Capabilities)>,
    /// A printer being asked what it can do.
    asking: Option<(String, Receiver<Capabilities>)>,
    /// Why printing is not possible on this system.
    unavailable: Option<String>,
    /// Save as PDF's file browser, and the folder it last saved in.
    browser: Option<Browser>,
    pdf_dir: Option<PathBuf>,
    /// *Print* was clicked and waits for the next tick, which has the page's inputs.
    want_print: bool,
    /// The worker making a printout, and what it was asked.
    output: Option<(Worker, u64)>,
    /// What the last printout did.
    status: Option<Result<String, String>>,
}

/// Where a printout goes.
enum Output {
    Preview,
    Pdf(PathBuf),
    Print(printer::Request),
}

impl PrintUi {
    pub fn new() -> Self {
        Self {
            kind: Kind::Map,
            label: String::new(),
            area: None,
            wide: false,
            drag: None,
            worker: None,
            back: Page::default(),
            seq: 0,
            seen: None,
            asked: None,
            preview: Preview::default(),
            big: None,
            printers: Vec::new(),
            listing: None,
            caps: None,
            asking: None,
            unavailable: None,
            browser: None,
            pdf_dir: None,
            want_print: false,
            output: None,
            status: None,
        }
    }

    /// Opens the page for `kind`: the map fitted to the route if there is one, else to what
    /// the map shows.
    pub fn open(&mut self, ctx: &egui::Context, kind: Kind, around: &Around, settings: &Settings) {
        self.kind = kind;
        self.big = None;
        self.drag = None;
        self.seen = None;
        self.asked = None;
        self.preview.signature = 0;
        self.status = None;
        self.unavailable = None;
        self.printers = Vec::new();
        self.caps = None;
        self.asking = None;
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        self.listing = Some(rx);
        let spawned = std::thread::Builder::new()
            .name("printers".into())
            .spawn(move || {
                log::info!("print: listing printers");
                let unavailable = printer::unavailable();
                let printers = if unavailable.is_none() {
                    printer::list()
                } else {
                    Vec::new()
                };
                log::info!("print: {} printers", printers.len());
                let _ = tx.send((unavailable, printers));
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.listing = None;
            self.unavailable = Some("Printers could not be listed.".into());
        }
        let [x0, y0, x1, y1] = around.cam.visible();
        self.wide = match around.route_bounds {
            Some(b) => b[2] - b[0] >= b[3] - b[1],
            None => x1 - x0 >= y1 - y0,
        };
        let layout = self.layout(settings);
        self.area = Some(self.initial_area(around, &layout));
    }

    fn initial_area(&self, around: &Around, layout: &Layout) -> Area {
        match around.route_bounds {
            Some(b) => Area::fit(b, layout.aspect(), 0.06),
            None => Area::of_view(around.cam, layout.aspect()),
        }
        .clamped(layout)
    }

    pub fn layout(&self, s: &Settings) -> Layout {
        Layout::new(
            s.paper,
            s.orientation.landscape(self.wide),
            !self.label.is_empty(),
            s.text,
        )
    }

    /// Escape closes the page, unless a text box has the keys.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if self.big.is_some() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.big = None;
                return true;
            }
            return false;
        }
        !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    }

    /// The page in the side panel.
    pub fn page_ui(&mut self, ui: &mut egui::Ui, settings: &mut Settings, around: &Around) {
        ui.add_space(4.0);
        self.poll_printers();
        let chosen = self.chosen_printer(settings);
        let (caps, caps_ready) = self.capabilities(ui.ctx(), chosen.as_deref());
        let loading = self.listing.is_some();
        egui::Grid::new("print-settings")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Content");
                if !around.has_route {
                    self.kind = Kind::Map;
                }
                ui.add_enabled_ui(around.has_route, |ui| {
                    egui::ComboBox::from_id_salt("print-content")
                        .selected_text(self.kind.name())
                        .show_ui(ui, |ui| {
                            for k in [Kind::Map, Kind::Directions] {
                                ui.selectable_value(&mut self.kind, k, k.name());
                            }
                        });
                });
                ui.end_row();
                ui.label("Label");
                ui.add(
                    egui::TextEdit::singleline(&mut self.label)
                        .desired_width(180.0)
                        .hint_text("Printed at the top of the page"),
                );
                ui.end_row();
                ui.label("Printer");
                ui.horizontal(|ui| {
                    if loading {
                        ui.spinner();
                        ui.label(egui::RichText::new("Loading printers…").color(WEAK));
                    } else if let Some(reason) = &self.unavailable {
                        ui.label(egui::RichText::new(reason).color(WEAK).size(12.0));
                    } else if self.printers.is_empty() {
                        ui.label(egui::RichText::new("No printers found").color(WEAK));
                    } else {
                        egui::ComboBox::from_id_salt("print-printer")
                            .selected_text(chosen.clone().unwrap_or_default())
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                for p in &self.printers {
                                    let on = chosen.as_deref() == Some(p.name.as_str());
                                    if ui.selectable_label(on, &p.name).clicked() {
                                        settings.printer = p.name.clone();
                                    }
                                }
                            });
                        if printer::HAS_PROPERTIES
                            && let Some(name) = &chosen
                            && ui
                                .button("Properties…")
                                .on_hover_text("The printer driver's own settings")
                                .clicked()
                            && let Err(e) = printer::properties(name)
                        {
                            self.status = Some(Err(e));
                        }
                    }
                });
                ui.end_row();
                ui.label("Paper");
                egui::ComboBox::from_id_salt("print-paper")
                    .selected_text(settings.paper.name())
                    .show_ui(ui, |ui| {
                        for p in Paper::ALL {
                            if caps.takes(p) || p == settings.paper {
                                ui.selectable_value(&mut settings.paper, p, p.name());
                            }
                        }
                    });
                ui.end_row();
                if self.kind == Kind::Map {
                    ui.label("Orientation");
                    egui::ComboBox::from_id_salt("print-orientation")
                        .selected_text(settings.orientation.name())
                        .show_ui(ui, |ui| {
                            for o in Orientation::ALL {
                                ui.selectable_value(&mut settings.orientation, o, o.name());
                            }
                        });
                    ui.end_row();
                }
                ui.label("Text size");
                let mut percent = settings.text.0;
                if ui
                    .add(egui::Slider::new(&mut percent, TextSize::MIN..=TextSize::MAX).suffix("%"))
                    .on_hover_text(
                        "How large the text and symbols print; smaller shows more ground",
                    )
                    .changed()
                {
                    settings.text = TextSize(percent);
                }
                ui.end_row();
                ui.label("Copies");
                ui.add(egui::DragValue::new(&mut settings.copies).range(1..=99));
                ui.end_row();
            });
        if self.kind == Kind::Map {
            let layout = self.layout(settings);
            if let Some(area) = &self.area {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(area.scale_text(&layout, around.units)).color(INK));
            }
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(around.has_route, egui::Button::new("Fit to route"))
                    .clicked()
                {
                    self.area = Some(self.initial_area(around, &layout));
                }
                if ui.button("Use the view").clicked() {
                    self.area = Some(Area::of_view(around.cam, layout.aspect()).clamped(&layout));
                }
            });
            ui.label(
                egui::RichText::new(
                    "Drag the rectangle on the map to move it, or a corner to resize it.",
                )
                .color(WEAK)
                .size(12.0),
            );
        }
        if chosen.is_some() && !caps.takes(settings.paper) {
            ui.label(
                egui::RichText::new(format!(
                    "The printer does not list {}.",
                    settings.paper.name()
                ))
                .color(Color32::from_rgb(180, 30, 30))
                .size(12.0),
            );
        }
        if let Some(m) = caps.margin_pt
            && m > crate::print::MARGIN_PT + 0.5
        {
            ui.label(
                egui::RichText::new(format!(
                    "This printer cannot print the outer {:.2} in; the page's edge is trimmed.",
                    m / 72.0
                ))
                .color(WEAK)
                .size(12.0),
            );
        }
        ui.add_space(6.0);
        self.preview_ui(ui);
        ui.add_space(6.0);
        let working = self.output.is_some();
        ui.horizontal_wrapped(|ui| {
            let can = self.unavailable.is_none()
                && chosen.is_some()
                && caps_ready
                && !loading
                && !working;
            let print = ui.add_enabled(can, egui::Button::new("Print"));
            let print = match (&self.unavailable, chosen.is_some()) {
                (Some(why), _) => print.on_disabled_hover_text(why),
                (None, false) => print.on_disabled_hover_text("There is no printer to print on"),
                _ => print,
            };
            if print.clicked() {
                self.want_print = true;
            }
            if ui
                .add_enabled(!working, egui::Button::new("Save as PDF…"))
                .clicked()
            {
                let dir = self.pdf_dir.clone().unwrap_or_else(exe_dir);
                let name = match (self.label.trim(), self.kind) {
                    ("", Kind::Map) => "Map".to_owned(),
                    ("", Kind::Directions) => "Directions".to_owned(),
                    (l, _) => files::file_name(l),
                };
                self.browser =
                    Some(Browser::new("Save as PDF", Mode::Save, &dir, &name).with_ext("pdf"));
            }
        });
        if working {
            ui.label(egui::RichText::new("Drawing the pages and sending them…").color(WEAK));
        }
        match &self.status {
            Some(Ok(m)) => {
                ui.label(egui::RichText::new(m).color(INK));
            }
            Some(Err(e)) => {
                ui.label(egui::RichText::new(e).color(Color32::from_rgb(180, 30, 30)));
            }
            None => {}
        }
    }

    /// The printer in use: the one chosen if it still exists, else the system's default, else
    /// the first.
    fn chosen_printer(&self, settings: &Settings) -> Option<String> {
        let by = |f: &dyn Fn(&Printer) -> bool| self.printers.iter().find(|p| f(p));
        by(&|p| p.name == settings.printer)
            .or_else(|| by(&|p| p.is_default))
            .or_else(|| self.printers.first())
            .map(|p| p.name.clone())
    }

    /// Takes the printer list from the thread that is reading it.
    fn poll_printers(&mut self) {
        if let Some(rx) = &self.listing
            && let Ok((unavailable, printers)) = rx.try_recv()
        {
            self.unavailable = unavailable;
            self.printers = printers;
            self.listing = None;
        }
    }

    /// What `printer` can do, asked once per printer on a thread of its own; all the sizes
    /// until it answers, and whether it has.
    fn capabilities(&mut self, ctx: &egui::Context, printer: Option<&str>) -> (Capabilities, bool) {
        let Some(name) = printer else {
            return (Capabilities::default(), true);
        };
        if let Some((n, rx)) = &self.asking
            && let Ok(c) = rx.try_recv()
        {
            log::info!("print: {n}: {c:?}");
            self.caps = Some((n.clone(), c));
            self.asking = None;
        }
        if let Some((n, c)) = &self.caps
            && n == name
        {
            return (c.clone(), true);
        }
        if self.asking.as_ref().is_none_or(|(n, _)| n != name) {
            log::info!("print: asking {name} what it can do");
            let (tx, rx) = mpsc::channel();
            let (owned, ctx) = (name.to_owned(), ctx.clone());
            let spawned = std::thread::Builder::new()
                .name("printer capabilities".into())
                .spawn(move || {
                    let _ = tx.send(printer::capabilities(&owned));
                    ctx.request_repaint();
                });
            if spawned.is_err() {
                return (Capabilities::default(), true);
            }
            self.asking = Some((name.to_owned(), rx));
        }
        (Capabilities::default(), false)
    }

    /// The thumbnail, or what is wrong.
    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self
            .asked
            .is_some_and(|(sig, _)| sig != self.preview.signature)
            || self
                .seen
                .is_some_and(|(sig, _)| sig != self.preview.signature);
        if let Some(e) = &self.preview.error {
            ui.label(
                egui::RichText::new(format!("The page could not be drawn: {e}"))
                    .color(Color32::from_rgb(180, 30, 30)),
            );
        }
        let Some(tex) = self.preview.pages.first() else {
            if busy {
                ui.label(egui::RichText::new("Drawing the page…").color(WEAK));
            }
            return;
        };
        let size = tex.size_vec2();
        // Never taller than what is left above the buttons under it.
        let room = (ui.available_height() - 40.0).max(60.0);
        let w = ui
            .available_width()
            .min(size.x.max(1.0) * 0.6)
            .min(room * size.x / size.y.max(1.0));
        let shown = Vec2::new(w, w * size.y / size.x);
        let (rect, resp) = ui.allocate_exact_size(shown, Sense::click());
        ui.painter()
            .rect_filled(rect.expand(2.0), 2.0, Color32::from_gray(180));
        ui.painter().image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if busy {
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_white_alpha(130));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Drawing the page…",
                egui::FontId::proportional(13.0),
                INK,
            );
        }
        if resp
            .on_hover_text("Click to see the page larger")
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked()
        {
            self.big = Some(0);
        }
    }

    /// The large preview over the map.
    pub fn big_window(&mut self, ctx: &egui::Context) {
        let Some(mut at) = self.big else { return };
        let n = self.preview.pages.len();
        if n == 0 {
            self.big = None;
            return;
        }
        at = at.min(n - 1);
        let mut open = true;
        let screen = ctx.content_rect();
        let height = (screen.height() - 120.0).max(200.0);
        egui::Window::new("Page preview")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if n > 1 {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(at > 0, egui::Button::new("‹ Previous"))
                            .clicked()
                        {
                            at -= 1;
                        }
                        ui.label(format!("Page {} of {n}", at + 1));
                        if ui
                            .add_enabled(at + 1 < n, egui::Button::new("Next ›"))
                            .clicked()
                        {
                            at += 1;
                        }
                    });
                }
                let tex = &self.preview.pages[at];
                let size = tex.size_vec2();
                let scale = (height / size.y)
                    .min((screen.width() - 60.0) / size.x)
                    .min(1.5);
                let shown = size * scale;
                let (rect, _) = ui.allocate_exact_size(shown, Sense::hover());
                ui.painter()
                    .rect_filled(rect.expand(1.0), 0.0, Color32::from_gray(150));
                ui.painter().image(
                    tex.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            });
        self.big = open.then_some(at);
    }

    /// Whether the rectangle is on the map: the print page shows a map page.
    pub fn shows_rectangle(&self) -> bool {
        self.kind == Kind::Map && self.area.is_some()
    }

    /// The area's corners on screen, `[x0, y0, x1, y1]`, for a map drawn at `origin`.
    fn corners(&self, cam: &Camera, origin: Pos2, layout: &Layout) -> Option<Rect> {
        let a = self.area?.rect(layout.aspect());
        let p = |w: [f64; 2]| {
            let s = cam.world_to_screen(w);
            origin + Vec2::new(s[0] as f32, s[1] as f32)
        };
        Some(Rect::from_two_pos(p([a[0], a[1]]), p([a[2], a[3]])))
    }

    /// Draws the rectangle over the map: the rest dimmed, a border, and handles at the
    /// corners.
    pub fn draw_rectangle(
        &self,
        painter: &egui::Painter,
        map: Rect,
        cam: &Camera,
        settings: &Settings,
    ) {
        let layout = self.layout(settings);
        let Some(r) = self.corners(cam, map.min, &layout) else {
            return;
        };
        let dim = Color32::from_black_alpha(95);
        let inside = r.intersect(map);
        if inside.is_positive() {
            painter.rect_filled(
                Rect::from_min_max(map.min, Pos2::new(map.max.x, inside.top())),
                0.0,
                dim,
            );
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(map.min.x, inside.bottom()), map.max),
                0.0,
                dim,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(map.min.x, inside.top()),
                    Pos2::new(inside.left(), inside.bottom()),
                ),
                0.0,
                dim,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(inside.right(), inside.top()),
                    Pos2::new(map.max.x, inside.bottom()),
                ),
                0.0,
                dim,
            );
        } else {
            painter.rect_filled(map, 0.0, dim);
        }
        painter.rect_stroke(r, 0.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
        for c in [
            r.left_top(),
            r.right_top(),
            r.left_bottom(),
            r.right_bottom(),
        ] {
            let h = Rect::from_center_size(c, Vec2::splat(9.0));
            painter.rect(
                h,
                1.0,
                Color32::WHITE,
                Stroke::new(1.5, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// Moves or resizes the rectangle with the pointer. Returns whether the rectangle has
    /// the pointer, so the map must not pan with it.
    pub fn pointer(
        &mut self,
        ctx: &egui::Context,
        resp: &Response,
        origin: Pos2,
        cam: &Camera,
        settings: &Settings,
    ) -> bool {
        let layout = self.layout(settings);
        let Some(area) = self.area else { return false };
        let Some(r) = self.corners(cam, origin, &layout) else {
            return false;
        };
        let (pos, pressed, down) = ctx.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
            )
        });
        let world =
            |p: Pos2| cam.screen_to_world([f64::from(p.x - origin.x), f64::from(p.y - origin.y)]);
        // The opposite corner, and whether the pair runs from top right to bottom left.
        let corner_at = |p: Pos2| {
            [
                (r.left_top(), r.right_bottom(), false),
                (r.right_top(), r.left_bottom(), true),
                (r.left_bottom(), r.right_top(), true),
                (r.right_bottom(), r.left_top(), false),
            ]
            .into_iter()
            .find(|(c, ..)| c.distance(p) <= GRAB)
            .map(|(_, opposite, anti)| (opposite, anti))
        };
        let resize = |anti: bool| match anti {
            true => CursorIcon::ResizeNeSw,
            false => CursorIcon::ResizeNwSe,
        };
        if self.drag.is_none() {
            let Some(p) = pos.filter(|_| resp.hovered()) else {
                return false;
            };
            let on_corner = corner_at(p);
            if let Some((_, anti)) = on_corner {
                ctx.set_cursor_icon(resize(anti));
            } else if r.contains(p) {
                ctx.set_cursor_icon(CursorIcon::Grab);
            }
            if !pressed {
                return false;
            }
            self.drag = if let Some((opposite, anti)) = on_corner {
                Some(Drag::Corner(world(opposite), anti))
            } else if r.contains(p) {
                let w = world(p);
                Some(Drag::Move([w[0] - area.center[0], w[1] - area.center[1]]))
            } else {
                None
            };
            return self.drag.is_some();
        }
        if !down {
            self.drag = None;
            return true;
        }
        let Some(p) = pos else { return true };
        let w = world(p);
        let area = match self.drag {
            Some(Drag::Move(grab)) => {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
                Area {
                    center: [w[0] - grab[0], w[1] - grab[1]],
                    width: area.width,
                }
            }
            Some(Drag::Corner(a, anti)) => {
                ctx.set_cursor_icon(resize(anti));
                let (dx, dy) = (w[0] - a[0], w[1] - a[1]);
                let aspect = layout.aspect();
                let [lo, hi] = Area::width_range(&layout);
                let width = dx.abs().max(dy.abs() / aspect).clamp(lo, hi);
                Area {
                    center: [
                        a[0] + dx.signum() * width / 2.0,
                        a[1] + dy.signum() * width * aspect / 2.0,
                    ],
                    width,
                }
            }
            None => area,
        };
        self.area = Some(area);
        true
    }

    /// Draws the preview again when what it is made from has held still for a moment, and
    /// takes the worker's answer.
    pub fn tick(&mut self, ctx: &egui::Context, inputs: &Inputs) {
        let layout = self.layout(&inputs.settings);
        // The area keeps its ground width and centre when the page changes shape; keep it
        // within the zooms the page can draw.
        if let Some(a) = self.area {
            self.area = Some(a.clamped(&layout));
        }
        let sig = self.signature(inputs);
        match self.seen {
            Some((s, _)) if s == sig => {}
            _ => self.seen = Some((sig, Instant::now())),
        }
        if let Some((s, since)) = self.seen
            && self.asked.is_none_or(|(a, _)| a != s)
            && s != self.preview.signature
        {
            let waited = since.elapsed();
            if waited >= DEBOUNCE {
                self.ask(ctx, inputs, s);
            } else {
                ctx.request_repaint_after(DEBOUNCE - waited);
            }
        }
        while let Some(reply) = self.worker.as_ref().and_then(Worker::try_reply) {
            if self.asked.is_none_or(|(_, seq)| seq != reply.seq) {
                continue;
            }
            let (sig, _) = self.asked.take().unwrap_or_default();
            self.preview.signature = sig;
            match reply.result {
                Ok(done) => {
                    self.preview.error = None;
                    self.preview.took = done.took;
                    self.preview.note = done.note;
                    self.preview.pages = done
                        .pages
                        .into_iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let img = egui::ColorImage::from_rgb(
                                [p.size[0] as usize, p.size[1] as usize],
                                &p.rgb,
                            );
                            ctx.load_texture(
                                format!("print-page-{i}"),
                                img,
                                egui::TextureOptions::LINEAR,
                            )
                        })
                        .collect();
                }
                Err(e) => {
                    self.preview.error = Some(e);
                    self.preview.pages.clear();
                }
            }
            ctx.request_repaint();
        }
        if self.asked.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.outputs(ctx, inputs);
    }

    /// The page as `dpi` draws it, or why it cannot be.
    fn make(&self, inputs: &Inputs, dpi: f64) -> Result<What, String> {
        let s = &inputs.settings;
        match self.kind {
            Kind::Map => {
                let layout = self.layout(s);
                let area = self.area.ok_or("no area chosen")?;
                Ok(What::Map(Box::new(MapPage {
                    layout,
                    area,
                    label: self.label.clone(),
                    units: inputs.units,
                    layers: inputs.layers,
                    dpi,
                    msaa: inputs.msaa,
                    lines: inputs.routing.print_lines(),
                    stops: inputs.routing.stop_marks(),
                    pins: inputs.pins.shown_pins(),
                })))
            }
            Kind::Directions => {
                let doc = inputs
                    .routing
                    .directions_doc(&self.label, inputs.units)
                    .ok_or("there is no route to print")?;
                Ok(What::Directions(Box::new(doc), s.paper, s.text, dpi))
            }
        }
    }

    fn ask(&mut self, ctx: &egui::Context, inputs: &Inputs, sig: u64) {
        let what = match self.make(inputs, PREVIEW_DPI) {
            Ok(w) => w,
            Err(e) => {
                self.preview.error = Some(e);
                self.preview.pages.clear();
                self.preview.signature = sig;
                return;
            }
        };
        self.seq += 1;
        self.asked = Some((sig, self.seq));
        self.worker.get_or_insert_with(Worker::start).send(Job {
            seq: self.seq,
            pack: inputs.pack.to_owned(),
            style: inputs.style.clone(),
            what,
            output: Output::Preview,
            ctx: ctx.clone(),
            gpu: inputs.gpu.clone(),
        });
    }

    /// Starts a printout: the pages drawn at the printer's resolution, one at a time, and
    /// sent to `output`.
    fn start_output(&mut self, ctx: &egui::Context, inputs: &Inputs, output: Output, dpi: f64) {
        let what = match self.make(inputs, dpi) {
            Ok(w) => w,
            Err(e) => {
                self.status = Some(Err(e));
                return;
            }
        };
        self.seq += 1;
        self.status = None;
        let worker = Worker::start();
        worker.send(Job {
            seq: self.seq,
            pack: inputs.pack.to_owned(),
            style: inputs.style.clone(),
            what,
            output,
            ctx: ctx.clone(),
            gpu: inputs.gpu.clone(),
        });
        self.output = Some((worker, self.seq));
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    /// Print was clicked, the file browser answered, or a printout finished.
    fn outputs(&mut self, ctx: &egui::Context, inputs: &Inputs) {
        if let Some((worker, seq)) = &self.output
            && let Some(reply) = worker.try_reply()
            && reply.seq == *seq
        {
            self.status = Some(reply.result.map(|d| d.note));
            self.output = None;
            ctx.request_repaint();
        } else if self.output.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if let Some(b) = &mut self.browser {
            match b.ui(ctx) {
                Some(Outcome::Chosen(path)) => {
                    self.browser = None;
                    self.pdf_dir = path.parent().map(Path::to_path_buf);
                    let dpi = printer::dpi_for(None);
                    self.start_output(ctx, inputs, Output::Pdf(path), dpi);
                }
                Some(Outcome::Cancelled) => self.browser = None,
                None => {}
            }
        }
        if std::mem::take(&mut self.want_print) && self.output.is_none() {
            let Some(name) = self.chosen_printer(&inputs.settings) else {
                return;
            };
            let (caps, _) = self.capabilities(ctx, Some(&name));
            let s = &inputs.settings;
            let landscape =
                self.kind == Kind::Map && self.layout(s).page[0] > self.layout(s).page[1];
            let request = printer::Request {
                printer: name,
                paper: s.paper,
                landscape,
                copies: s.copies,
                title: if self.label.trim().is_empty() {
                    "Offline Road Maps".into()
                } else {
                    self.label.trim().to_owned()
                },
            };
            // One submit of a larger page holds wgpu's GL context for over a second, and the
            // window thread then panics waiting for it (the E6430 at 600 dpi, step 4.11).
            let gl = inputs
                .gpu
                .as_ref()
                .is_some_and(|(a, _, _)| a.get_info().backend == eframe::wgpu::Backend::Gl);
            let dpi = printer::dpi_for(caps.dpi);
            self.start_output(
                ctx,
                inputs,
                Output::Print(request),
                if gl { dpi.min(GL_MAX_DPI) } else { dpi },
            );
        }
    }

    /// The preview is up to date and drawn, or could not be (the bench).
    pub fn bench_previewed(&self) -> Option<Result<(), String>> {
        let current =
            self.asked.is_none() && self.seen.is_some_and(|(s, _)| s == self.preview.signature);
        match (current, &self.preview.error) {
            (false, _) => None,
            (true, Some(e)) => Some(Err(e.clone())),
            (true, None) => Some(Ok(())),
        }
    }

    /// Starts *Save as PDF* to `path` at `dpi` (the bench).
    pub fn bench_start(&mut self, ctx: &egui::Context, inputs: &Inputs, path: PathBuf, dpi: f64) {
        self.start_output(ctx, inputs, Output::Pdf(path), dpi);
    }

    /// How the printout started by [`Self::bench_start`] ended, once it has.
    pub fn bench_finished(&mut self) -> Option<Result<(), String>> {
        match self.output {
            None => self.status.take().map(|r| r.map(drop)),
            Some(_) => None,
        }
    }

    /// What the page is made from, as a number: when it changes the preview is out of date.
    fn signature(&self, inputs: &Inputs) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let s = &inputs.settings;
        (self.kind as u8, &self.label, s.paper as u8, s.text.0).hash(&mut h);
        s.orientation.landscape(self.wide).hash(&mut h);
        inputs.units.name().hash(&mut h);
        inputs.pack.hash(&mut h);
        format!("{:?}", inputs.layers).hash(&mut h);
        if self.kind == Kind::Map
            && let Some(a) = self.area
        {
            [a.center[0], a.center[1], a.width]
                .map(f64::to_bits)
                .hash(&mut h);
        }
        for (look, line) in inputs.routing.print_lines() {
            (look as u8, line.points).hash(&mut h);
            line.bounds.map(f64::to_bits).hash(&mut h);
        }
        for m in inputs.routing.stop_marks() {
            (m.p.map(f64::to_bits), &m.label).hash(&mut h);
        }
        inputs.pins.shown_count().hash(&mut h);
        if self.kind == Kind::Directions
            && let Some(d) = inputs.routing.directions_doc("", inputs.units)
        {
            (d.steps.len(), &d.totals, d.notes.len()).hash(&mut h);
            d.step_times
                .iter()
                .map(|t| t.as_ref().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .hash(&mut h);
        }
        h.finish()
    }
}

enum What {
    Map(Box<MapPage>),
    /// The document, its paper and text size, and the dpi.
    Directions(Box<DirectionsDoc>, Paper, TextSize, f64),
}

/// Where the pages of a job go as they are drawn.
enum Sink {
    Collect(Vec<Printed>),
    Pdf(Pdf<std::io::BufWriter<std::fs::File>>, PathBuf),
    Spool(printer::Spool),
}

impl Sink {
    fn new(output: Output, title: &str) -> Result<Self, String> {
        Ok(match output {
            Output::Preview => Self::Collect(Vec::new()),
            Output::Pdf(path) => {
                let file = std::fs::File::create(&path)
                    .map_err(|e| files::save_error(path.parent().unwrap_or(Path::new(".")), &e))?;
                let title = if title.is_empty() {
                    "Offline Road Maps"
                } else {
                    title
                };
                let pdf = Pdf::new(std::io::BufWriter::new(file), title)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Self::Pdf(pdf, path)
            }
            Output::Print(request) => Self::Spool(printer::Spool::open(&request)?),
        })
    }

    fn add(&mut self, page: Printed) -> Result<(), String> {
        match self {
            Self::Collect(v) => {
                v.push(page);
                Ok(())
            }
            Self::Pdf(pdf, path) => pdf
                .page(&page.rgb, page.size, page.size_pt)
                .map_err(|e| format!("{}: {e}", path.display())),
            Self::Spool(spool) => spool.page(&page),
        }
    }

    /// Closes the document; the pages kept (a preview's) and what to tell the user.
    fn finish(self) -> Result<(Vec<Printed>, String), String> {
        match self {
            Self::Collect(v) => Ok((v, String::new())),
            Self::Pdf(pdf, path) => {
                let w = pdf
                    .finish()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                w.into_inner()
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .sync_all()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok((Vec::new(), format!("Saved {}.", path.display())))
            }
            Self::Spool(spool) => Ok((Vec::new(), spool.finish()?)),
        }
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

struct Job {
    seq: u64,
    pack: PathBuf,
    style: StyleFile,
    what: What,
    output: Output,
    ctx: egui::Context,
    gpu: Option<crate::snapshot::Shared>,
}

struct Done {
    pages: Vec<Printed>,
    took: Duration,
    note: String,
}

struct Reply {
    seq: u64,
    result: Result<Done, String>,
}

/// The thread that draws the previews: it keeps its own GPU device, the open pack and
/// egui's renderer, and draws only the latest request.
struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Reply>,
}

impl Worker {
    fn start() -> Self {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (replies, rx) = mpsc::channel::<Reply>();
        let spawned = std::thread::Builder::new()
            .name("print preview".into())
            // Drawing a page nests deeply; Windows threads start small.
            .stack_size(32 << 20)
            .spawn(move || {
                let mut state = WorkerState::default();
                while let Ok(mut job) = jobs.recv() {
                    // Only the latest: older requests are out of date.
                    while let Ok(newer) = jobs.try_recv() {
                        job = newer;
                    }
                    let (seq, ctx) = (job.seq, job.ctx.clone());
                    log::info!("print worker: job {seq}");
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.run(job)))
                            .unwrap_or_else(|_| Err("the page drawing thread panicked".into()));
                    log::info!("print worker: job {seq} done, ok = {}", result.is_ok());
                    if replies.send(Reply { seq, result }).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            eprintln!("print preview: no thread: {e}");
        }
        Self { tx, rx }
    }

    fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    fn try_reply(&self) -> Option<Reply> {
        self.rx.try_recv().ok()
    }
}

#[derive(Default)]
struct WorkerState {
    gpu: Option<(Gpu, Egui)>,
    pack: Option<(PathBuf, packs::Opened)>,
}

/// Set when the window is closing.
pub static EXITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Drop for WorkerState {
    /// While the window closes, a worker that frees its GPU resources makes the GL backend
    /// take the context from the window's thread ("BadAccess") and abort; the process is
    /// ending anyway, so keep them.
    fn drop(&mut self) {
        if EXITING.load(std::sync::atomic::Ordering::Relaxed) {
            std::mem::forget(self.gpu.take());
        }
    }
}

impl WorkerState {
    fn run(&mut self, job: Job) -> Result<Done, String> {
        let t0 = Instant::now();
        if self.gpu.is_none() {
            let gpu = match job.gpu.clone() {
                Some(shared) if std::env::var_os("OMP_PRINT_OWN_DEVICE").is_none() => {
                    log::info!("print worker: using the window's GPU device");
                    Gpu::shared(shared)
                }
                _ => {
                    log::info!("print worker: opening a GPU device");
                    Gpu::headless()?
                }
            };
            log::info!("print worker: device ready");
            let egui_pass = Egui::new(&gpu);
            self.gpu = Some((gpu, egui_pass));
        }
        if self.pack.as_ref().is_none_or(|(p, _)| *p != job.pack) {
            log::info!("print worker: opening {}", job.pack.display());
            self.pack = Some((job.pack.clone(), packs::open(&job.pack)?));
            log::info!("print worker: pack open");
        }
        let (gpu, egui_pass) = self.gpu.as_mut().ok_or("no GPU")?;
        let opened = &self.pack.as_ref().ok_or("no pack")?.1;
        let (style, _) = Style::compile(&job.style, &opened.schema);
        let mut note = String::new();
        let title = match &job.what {
            What::Map(p) => p.label.clone(),
            What::Directions(d, ..) => d.label.clone(),
        };
        let mut sink = Sink::new(job.output, &title)?;
        match job.what {
            What::Map(page) => {
                log::info!("print worker: drawing the map page at {} dpi", page.dpi);
                let (p, stats) = pages::render_map(gpu, opened, &style, egui_pass, &page)?;
                log::info!("print worker: map page drawn");
                note = format!(
                    "z{:.1}, {} tiles, map {:.0} ms, overlay {:.0} ms",
                    stats.z, stats.tiles, stats.map_ms, stats.egui_ms
                );
                sink.add(p)?;
            }
            What::Directions(doc, paper, text, dpi) => {
                pages::render_directions(gpu, egui_pass, &doc, paper, text, dpi, &mut |p| {
                    sink.add(p)
                })?;
            }
        }
        let (pages, said) = sink.finish()?;
        if !said.is_empty() {
            note = said;
        }
        Ok(Done {
            pages,
            took: t0.elapsed(),
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, PointerButton};

    /// The print page's rectangle on a 1000 x 800 map at (0, 0), driven by pointer events.
    struct Rig {
        ctx: egui::Context,
        cam: Camera,
        print: PrintUi,
        settings: Settings,
        held: bool,
    }

    const MAP: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));

    impl Rig {
        fn new() -> Self {
            let mut cam = Camera::new([0.0, 0.0, 1.0, 1.0]);
            cam.size = [1000.0, 800.0];
            cam.zoom = 10.0;
            cam.center = [0.3, 0.4];
            let settings = Settings::default();
            let mut print = PrintUi::new();
            print.open(
                &egui::Context::default(),
                Kind::Map,
                &Around {
                    cam: &cam,
                    units: Units::Miles,
                    has_route: false,
                    route_bounds: None,
                },
                &settings,
            );
            // Half the view's width, in the middle.
            let width = (cam.visible()[2] - cam.visible()[0]) / 2.0;
            print.area = Some(Area {
                center: cam.center,
                width,
            });
            Self {
                ctx: egui::Context::default(),
                cam,
                print,
                settings,
                held: false,
            }
        }

        fn frame(&mut self, events: Vec<Event>) {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(MAP),
                focused: true,
                ..Default::default()
            };
            let mut full = self.ctx.clone().run_ui(raw, |ui| {
                let resp = ui.allocate_rect(MAP, Sense::click_and_drag());
                self.held = self
                    .print
                    .pointer(ui.ctx(), &resp, MAP.min, &self.cam, &self.settings);
            });
            full.textures_delta.clear();
        }

        fn button(&mut self, pos: Pos2, pressed: bool) {
            self.frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }

        /// A drag from `from` to `to`, a frame for each step.
        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.frame(vec![Event::PointerMoved(from)]);
            self.button(from, true);
            self.frame(vec![Event::PointerMoved((from + to.to_vec2()) / 2.0)]);
            self.frame(vec![Event::PointerMoved(to)]);
            self.button(to, false);
        }

        fn area(&self) -> Area {
            self.print.area.unwrap()
        }

        fn screen(&self) -> Rect {
            let layout = self.print.layout(&self.settings);
            self.print.corners(&self.cam, MAP.min, &layout).unwrap()
        }
    }

    #[test]
    fn a_drag_inside_moves_the_rectangle() {
        let mut r = Rig::new();
        let (before, rect) = (r.area(), r.screen());
        r.drag(rect.center(), rect.center() + Vec2::new(60.0, -40.0));
        let after = r.area();
        assert_eq!(after.width, before.width);
        let per_pt = 1.0 / r.cam.scale();
        assert!((after.center[0] - before.center[0] - 60.0 * per_pt).abs() < 1e-9);
        assert!((after.center[1] - before.center[1] + 40.0 * per_pt).abs() < 1e-9);
        assert!(r.print.drag.is_none(), "the pointer is let go");
    }

    #[test]
    fn a_drag_on_a_corner_resizes_it_about_the_opposite_one() {
        let mut r = Rig::new();
        let rect = r.screen();
        let layout = r.print.layout(&r.settings);
        let before = r.area();
        r.drag(
            rect.right_bottom(),
            rect.right_bottom() + Vec2::new(100.0, 10.0),
        );
        let after = r.area();
        assert!(after.width > before.width);
        // The top-left corner has not moved, and the shape is the page's.
        let (w0, w1) = (before.rect(layout.aspect()), after.rect(layout.aspect()));
        assert!((w0[0] - w1[0]).abs() < 1e-9 && (w0[1] - w1[1]).abs() < 1e-9);
        let new = r.screen();
        assert!((new.width() / new.height() - 1.0 / layout.aspect() as f32).abs() < 1e-3);
        // Dragged up and to the left past the opposite corner: the same, mirrored.
        let mut r = Rig::new();
        let rect = r.screen();
        r.drag(rect.left_top(), rect.left_top() - Vec2::new(50.0, 50.0));
        let after = r.area();
        let w = after.rect(layout.aspect());
        let b = before.rect(layout.aspect());
        assert!((w[2] - b[2]).abs() < 1e-9 && (w[3] - b[3]).abs() < 1e-9);
        assert!(after.width > before.width);
    }

    #[test]
    fn a_drag_outside_is_the_maps() {
        let mut r = Rig::new();
        let before = r.area();
        r.drag(Pos2::new(20.0, 20.0), Pos2::new(120.0, 90.0));
        assert_eq!(r.area(), before);
        assert!(!r.held);
    }

    #[test]
    fn the_rectangle_keeps_the_pages_zooms() {
        let mut r = Rig::new();
        let rect = r.screen();
        // Drag a corner far past the opposite one: the width stays within what a page can show.
        r.drag(rect.right_bottom(), rect.left_top() - Vec2::new(5.0, 5.0));
        let layout = r.print.layout(&r.settings);
        let [lo, hi] = Area::width_range(&layout);
        assert!((lo..=hi).contains(&r.area().width));
    }

    #[test]
    fn the_signature_follows_what_the_page_is_made_from() {
        let mut r = Rig::new();
        let routing = RouteUi::stub();
        let pins = PinsUi::empty(PathBuf::from("pins"));
        let style = StyleFile::builtin();
        let inputs = |settings: Settings| Inputs {
            gpu: None,
            pack: Path::new("pack.omp.000"),
            style: &style,
            layers: Layers::default(),
            units: Units::Miles,
            msaa: 1,
            settings,
            routing: &routing,
            pins: &pins,
        };
        let a = r.print.signature(&inputs(r.settings.clone()));
        assert_eq!(a, r.print.signature(&inputs(r.settings.clone())));
        let mut big = r.settings.clone();
        big.text = TextSize(130);
        assert_ne!(a, r.print.signature(&inputs(big)));
        // Copies are for printing, not for the page.
        let mut two = r.settings.clone();
        two.copies = 2;
        assert_eq!(a, r.print.signature(&inputs(two)));
        r.print.label = "Summer trip".into();
        assert_ne!(a, r.print.signature(&inputs(r.settings.clone())));
        let b = r.print.signature(&inputs(r.settings.clone()));
        let rect = r.screen();
        r.drag(rect.center(), rect.center() + Vec2::new(5.0, 5.0));
        assert_ne!(b, r.print.signature(&inputs(r.settings.clone())));
    }
}
