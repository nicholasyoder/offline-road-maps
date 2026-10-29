//! Pushpins (phase 4 step 4.7): marked places with a name, a note and a symbol (a shape in a
//! colour), in sets, one TOML file per set in `pins/` next to the executable.
//!
//! A pushpin is added from the map's right-click menu, or from "what's here" and a picked
//! search result. The map renderer draws them as instanced symbols over the labels, from a
//! buffer made again only when the pins change, so a frame costs one draw call however many
//! there are (criterion 5: 10,000 pins within 0.1 ms of frame p95). Their names are drawn
//! from [`NAMES_ZOOM`] on, for the pins in view, under the route's markers. A click on one
//! opens it at the top of the Search page, with its note and the route buttons. Search finds
//! them by name or note, in memory, above the pack's results.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use omp_core::geo::{self, P, Unit};
use serde::{Deserialize, Serialize};

use crate::camera::{self, Camera};
use crate::files::{self, Saved};
use crate::pool::{Alloc, Kind, Pool};
use crate::routing::{self, MenuState, Place, StopAction};
use crate::style::{PIN_PAINTS, Rgba};
use crate::theme::{APPROX, Glyph, INK, WEAK, glyph_button};

/// The folder next to the executable.
pub const DIR: &str = "pins";
/// The set file format's version.
pub const VERSION: u32 = 1;
/// The set a first pushpin goes to.
const FIRST_SET: &str = "Pushpins";
/// A symbol's half size, in points; its white edge is inside it.
pub const HALF: f32 = 6.5;
pub const EDGE: f32 = 1.5;
/// Names are drawn from this zoom on, for at most this many pins in view.
pub const NAMES_ZOOM: f64 = 12.0;
const MAX_NAMES: usize = 200;
/// How near a click must be to a pin's centre, in points.
const CLICK_PT: f64 = HALF as f64 + 3.0;
/// Pins listed above the search's results.
const MAX_FOUND: usize = 6;
/// The zoom a pin opened from search or the Trips page is shown at, at least.
const SHOW_ZOOM: f64 = 15.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Circle,
    Square,
    Diamond,
    Triangle,
}

impl Shape {
    pub const ALL: [Self; 4] = [Self::Circle, Self::Square, Self::Diamond, Self::Triangle];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colour {
    #[default]
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Colour {
    pub const ALL: [Self; 6] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
    ];

    pub fn rgba(self) -> Rgba {
        match self {
            Self::Red => [214, 48, 49, 255],
            Self::Orange => [235, 128, 20, 255],
            Self::Yellow => [232, 190, 20, 255],
            Self::Green => [38, 150, 72, 255],
            Self::Blue => [36, 104, 224, 255],
            Self::Purple => [136, 70, 190, 255],
        }
    }

    fn color32(self) -> Color32 {
        let [r, g, b, _] = self.rgba();
        Color32::from_rgb(r, g, b)
    }
}

/// A pushpin, as the app holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub name: String,
    pub note: String,
    pub p: P,
    pub shape: Shape,
    pub colour: Colour,
}

/// A pushpin as its set's file holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinFile {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub shape: Shape,
    #[serde(default)]
    pub colour: Colour,
}

/// A set's file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetFile {
    pub version: u32,
    /// Drawn on the map and found by search.
    #[serde(default = "yes")]
    pub shown: bool,
    #[serde(default, rename = "pin")]
    pub pins: Vec<PinFile>,
}

fn yes() -> bool {
    true
}

impl From<&Pin> for PinFile {
    fn from(p: &Pin) -> Self {
        let (lat, lon) = geo::unproject(p.p);
        Self {
            name: p.name.clone(),
            note: p.note.clone(),
            lat,
            lon,
            shape: p.shape,
            colour: p.colour,
        }
    }
}

impl From<&PinFile> for Pin {
    fn from(f: &PinFile) -> Self {
        Self {
            name: f.name.clone(),
            note: f.note.clone(),
            p: geo::project(f.lat, f.lon),
            shape: f.shape,
            colour: f.colour,
        }
    }
}

/// A set of pushpins and its file.
#[derive(Clone, Debug)]
pub struct Set {
    pub name: String,
    pub path: PathBuf,
    pub shown: bool,
    pub pins: Vec<Pin>,
    /// Why it could not be saved last.
    pub error: Option<String>,
    /// Its pins are listed on the Trips page.
    pub expanded: bool,
}

impl Set {
    fn file(&self) -> SetFile {
        SetFile {
            version: VERSION,
            shown: self.shown,
            pins: self.pins.iter().map(PinFile::from).collect(),
        }
    }

    fn read(s: &Saved) -> Result<Self, String> {
        let f: SetFile = files::read(&s.path)?;
        if f.version > VERSION {
            return Err(format!(
                "{}: saved by a newer version of the app (format {})",
                s.path.display(),
                f.version
            ));
        }
        Ok(Self {
            name: s.name.clone(),
            path: s.path.clone(),
            shown: f.shown,
            pins: f.pins.iter().map(Pin::from).collect(),
            error: None,
            expanded: false,
        })
    }
}

/// One pin as the map shader draws it: its centre relative to the buffer's origin, in world
/// units, and its paint ([`PIN_PAINTS`] on) and shape.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct PinInstance {
    pub pos: [f32; 2],
    pub look: [u16; 2],
}

/// The shown pins on the GPU.
pub struct GpuPins {
    pub instances: Alloc,
    pub n: u32,
    /// The world point the positions are relative to, so `f32` keeps them to a fraction of a
    /// pixel at any zoom.
    origin: Unit,
}

/// The pins as a frame draws them: the buffer, its origin in viewport pixels, pixels per
/// world unit, and the symbols' half size in pixels.
pub struct PinDraw {
    pub gpu: Arc<GpuPins>,
    pub origin: [f32; 2],
    pub scale: f32,
    pub half: f32,
}

/// A pin: its set and its place in it.
pub type PinRef = (usize, usize);

/// The pushpin sets, the pin open on the Search page, and what the map draws.
pub struct PinsUi {
    pub dir: PathBuf,
    pub sets: Vec<Set>,
    /// The set new pins go to.
    pub current: usize,
    /// The pin open on the Search page.
    pub open: Option<PinRef>,
    /// A pin was opened since the side panel last looked: it shows the Search page.
    pub opened: bool,
    /// The open pin's name field takes the focus on the next frame (a new pin).
    focus_name: bool,
    /// A set whose pin's name or note is being typed: saved once the field is left.
    pending: Option<usize>,
    gpu: Option<Arc<GpuPins>>,
    /// The pins changed since the buffer was made.
    stale: bool,
    /// Counts the changes to the pins, so search's last answer is kept until they change.
    version: u64,
    /// The search box's text, the version and the pins found then.
    found: Option<(String, u64, Vec<PinRef>)>,
    /// Sets that could not be read, and other faults to show on the Trips page.
    pub errors: Vec<String>,
    /// The set being renamed, and the name typed.
    pub renaming: Option<(usize, String)>,
    /// The set whose Delete was clicked once.
    pub deleting: Option<usize>,
    /// Where the open pin's buttons were drawn, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) buttons: Vec<(&'static str, Rect)>,
}

impl PinsUi {
    /// No sets, in `dir` (the bench's, and the tests').
    pub fn empty(dir: PathBuf) -> Self {
        Self {
            dir,
            sets: Vec::new(),
            current: 0,
            open: None,
            opened: false,
            focus_name: false,
            pending: None,
            gpu: None,
            stale: true,
            version: 0,
            found: None,
            errors: Vec::new(),
            renaming: None,
            deleting: None,
            buttons: Vec::new(),
        }
    }

    /// The sets in `pins/` next to the executable.
    pub fn load(exe_dir: &Path) -> Self {
        let mut ui = Self::empty(exe_dir.join(DIR));
        match files::list(&ui.dir) {
            Ok(list) => {
                for s in &list {
                    match Set::read(s) {
                        Ok(set) => ui.sets.push(set),
                        Err(e) => ui.errors.push(e),
                    }
                }
            }
            Err(e) => ui.errors.push(format!("{}: {e}", ui.dir.display())),
        }
        ui
    }

    /// The pins or sets changed: their buffer and search's answer are made again.
    fn changed(&mut self) {
        self.stale = true;
        self.version += 1;
    }

    /// Puts `sets` in place of the sets shown (the bench's).
    pub fn replace(&mut self, sets: Vec<Set>) {
        self.sets = sets;
        self.open = None;
        self.changed();
    }

    pub fn count(&self) -> usize {
        self.sets.iter().map(|s| s.pins.len()).sum()
    }

    pub fn pin(&self, (s, i): PinRef) -> Option<&Pin> {
        self.sets.get(s)?.pins.get(i)
    }

    fn pin_mut(&mut self, (s, i): PinRef) -> Option<&mut Pin> {
        self.sets.get_mut(s)?.pins.get_mut(i)
    }

    /// The pins changed: draw them again, and keep them.
    fn touched(&mut self, set: usize) {
        self.changed();
        self.save(set);
    }

    /// Saves the set whose pin was being typed into, if any.
    pub fn flush(&mut self) {
        if let Some(i) = self.pending.take() {
            self.save(i);
        }
    }

    /// Writes set `i`'s file; a fault is kept with the set and shown.
    pub fn save(&mut self, i: usize) {
        let Some(s) = self.sets.get_mut(i) else {
            return;
        };
        s.error = files::write(&s.path, &s.file())
            .err()
            .map(|e| files::save_error(s.path.parent().unwrap_or(Path::new(".")), &e));
    }

    /// Saves set `i` to `path` from now on (the file browser's choice).
    pub fn save_as(&mut self, i: usize, path: PathBuf) {
        let Some(s) = self.sets.get_mut(i) else {
            return;
        };
        s.name = files::stem(&path);
        s.path = path;
        self.save(i);
    }

    /// Adds the set in `path` (from the file browser).
    pub fn open_file(&mut self, path: PathBuf) {
        if self.sets.iter().any(|s| s.path == path) {
            return;
        }
        let saved = Saved {
            name: files::stem(&path),
            path,
        };
        match Set::read(&saved) {
            Ok(s) => {
                self.sets.push(s);
                self.changed();
            }
            Err(e) => self.errors.push(e),
        }
    }

    /// A new, empty set, named after `name` or "name 2", … if that is taken.
    pub fn new_set(&mut self, name: &str) -> usize {
        let taken = |n: &str| {
            self.sets.iter().any(|s| s.name.eq_ignore_ascii_case(n))
                || files::path_for(&self.dir, n).exists()
        };
        let name = (1..)
            .map(|k| match k {
                1 => name.to_string(),
                k => format!("{name} {k}"),
            })
            .find(|n| !taken(n))
            .unwrap_or_default();
        self.sets.push(Set {
            path: files::path_for(&self.dir, &name),
            name,
            shown: true,
            pins: Vec::new(),
            error: None,
            expanded: false,
        });
        let i = self.sets.len() - 1;
        self.save(i);
        i
    }

    /// Adds a pin named `name` at `p` to the current set (made if there is none), and opens
    /// it on the Search page with its name ready to type over.
    pub fn add(&mut self, name: &str, p: P) -> PinRef {
        if self.current >= self.sets.len() {
            self.current = self
                .sets
                .iter()
                .position(|s| s.shown)
                .unwrap_or_else(|| self.new_set(FIRST_SET));
        }
        let set = &mut self.sets[self.current];
        set.shown = true;
        // A new pin looks like the last one added to its set.
        let (shape, colour) = set
            .pins
            .last()
            .map_or((Shape::Circle, Colour::Red), |l| (l.shape, l.colour));
        set.pins.push(Pin {
            name: name.into(),
            note: String::new(),
            p,
            shape,
            colour,
        });
        let r = (self.current, set.pins.len() - 1);
        self.touched(self.current);
        self.open = Some(r);
        self.opened = true;
        self.focus_name = true;
        r
    }

    /// The name a new pin at an unnamed point gets.
    pub fn next_name(&self) -> String {
        format!("Pushpin {}", self.count() + 1)
    }

    pub fn remove(&mut self, (s, i): PinRef) {
        let Some(set) = self.sets.get_mut(s) else {
            return;
        };
        if i < set.pins.len() {
            set.pins.remove(i);
            self.open = None;
            self.touched(s);
        }
    }

    /// Moves a pin to set `to`; returns where it is now.
    fn move_to(&mut self, (s, i): PinRef, to: usize) -> PinRef {
        if s == to || to >= self.sets.len() || i >= self.sets[s].pins.len() {
            return (s, i);
        }
        let pin = self.sets[s].pins.remove(i);
        self.sets[to].pins.push(pin);
        self.sets[to].shown = true;
        self.touched(s);
        self.touched(to);
        self.current = to;
        (to, self.sets[to].pins.len() - 1)
    }

    pub fn set_shown(&mut self, i: usize, shown: bool) {
        if self.sets.get(i).is_some_and(|s| s.shown != shown) {
            self.sets[i].shown = shown;
            if !shown && self.open.is_some_and(|o| o.0 == i) {
                self.open = None;
            }
            self.touched(i);
        }
    }

    pub fn rename_set(&mut self, i: usize, name: &str) -> Result<(), String> {
        let s = self.sets.get_mut(i).ok_or("no such set")?;
        s.path = files::rename(&s.path, name).map_err(|e| e.to_string())?;
        s.name = files::stem(&s.path);
        Ok(())
    }

    pub fn delete_set(&mut self, i: usize) -> Result<(), String> {
        let Some(s) = self.sets.get(i) else {
            return Ok(());
        };
        match std::fs::remove_file(&s.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", s.path.display())),
        }
        self.sets.remove(i);
        self.open = None;
        self.current = self.current.min(self.sets.len().saturating_sub(1));
        self.changed();
        Ok(())
    }

    /// The shown pins, with where each is.
    fn shown(&self) -> impl Iterator<Item = (PinRef, &Pin)> {
        self.sets
            .iter()
            .enumerate()
            .filter(|(_, s)| s.shown)
            .flat_map(|(si, s)| s.pins.iter().enumerate().map(move |(i, p)| ((si, i), p)))
    }

    /// The shown pin at `at` (points in the map), the nearest within reach.
    pub fn hit(&self, cam: &Camera, at: camera::Vec2) -> Option<PinRef> {
        let w = cam.screen_to_world(at);
        let reach = CLICK_PT / cam.scale();
        let mut best: Option<(f64, PinRef)> = None;
        for (r, pin) in self.shown() {
            let q = geo::to_unit(pin.p);
            let d = (q[0] - w[0]).hypot(q[1] - w[1]);
            if d <= reach && best.is_none_or(|b| d < b.0) {
                best = Some((d, r));
            }
        }
        best.map(|b| b.1)
    }

    /// Opens pin `r` on the Search page.
    pub fn show(&mut self, r: PinRef) {
        self.flush();
        self.open = Some(r);
        self.opened = true;
    }

    /// The view that shows pin `r`, zoomed in to at least [`SHOW_ZOOM`].
    pub fn view_of(&self, r: PinRef, cam: &Camera) -> Option<(camera::Vec2, f64)> {
        let p = self.pin(r)?;
        Some((geo::to_unit(p.p), cam.zoom.max(SHOW_ZOOM)))
    }

    /// The shown pins whose name or note holds `text`, case aside: names first.
    pub fn find(&self, text: &str) -> Vec<PinRef> {
        let t = text.trim().to_lowercase();
        if t.is_empty() {
            return Vec::new();
        }
        let mut by_name = Vec::new();
        let mut by_note = Vec::new();
        for (r, p) in self.shown() {
            if p.name.to_lowercase().contains(&t) {
                by_name.push(r);
            } else if p.note.to_lowercase().contains(&t) {
                by_note.push(r);
            }
        }
        by_name.extend(by_note);
        by_name.truncate(MAX_FOUND);
        by_name
    }

    /// The pins for the map renderer, the buffer made again if they changed.
    pub fn layout(&mut self, pool: &Arc<Pool>, cam: &Camera, ppp: f32) -> Option<PinDraw> {
        if self.stale {
            self.stale = false;
            self.gpu = self.upload(pool).map(Arc::new);
        }
        let gpu = self.gpu.clone()?;
        let ppp64 = f64::from(ppp);
        let o = cam.world_to_screen(gpu.origin);
        Some(PinDraw {
            origin: [(o[0] * ppp64) as f32, (o[1] * ppp64) as f32],
            scale: (cam.scale() * ppp64) as f32,
            half: HALF * ppp,
            gpu,
        })
    }

    fn upload(&self, pool: &Arc<Pool>) -> Option<GpuPins> {
        let points: Vec<(Unit, &Pin)> = self.shown().map(|(_, p)| (geo::to_unit(p.p), p)).collect();
        if points.is_empty() {
            return None;
        }
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for (q, _) in &points {
            b = [
                b[0].min(q[0]),
                b[1].min(q[1]),
                b[2].max(q[0]),
                b[3].max(q[1]),
            ];
        }
        let origin = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
        let instances: Vec<PinInstance> = points
            .iter()
            .map(|(q, p)| PinInstance {
                pos: [(q[0] - origin[0]) as f32, (q[1] - origin[1]) as f32],
                look: [(PIN_PAINTS + p.colour as usize) as u16, p.shape as u16],
            })
            .collect();
        Some(GpuPins {
            instances: pool.upload(Kind::Vertex, bytemuck::cast_slice(&instances)),
            n: instances.len() as u32,
            origin,
        })
    }

    /// The names of the pins in view, from [`NAMES_ZOOM`] on, beside their symbols.
    pub fn draw_names(&self, painter: &egui::Painter, origin: Pos2, cam: &Camera) {
        if cam.zoom < NAMES_ZOOM {
            return;
        }
        let [x0, y0, x1, y1] = cam.visible();
        let m = 100.0 / cam.scale();
        let mut drawn = 0;
        for (_, pin) in self.shown() {
            let q = geo::to_unit(pin.p);
            if q[0] < x0 - m || q[0] > x1 + m || q[1] < y0 || q[1] > y1 + m {
                continue;
            }
            if pin.name.is_empty() {
                continue;
            }
            let s = cam.world_to_screen(q);
            let at = Pos2::new(origin.x + s[0] as f32 + HALF + 3.0, origin.y + s[1] as f32);
            let galley = painter.layout_no_wrap(pin.name.clone(), FontId::proportional(12.0), INK);
            let rect = Rect::from_min_size(
                at - Vec2::new(2.0, galley.size().y / 2.0),
                galley.size() + Vec2::new(4.0, 0.0),
            );
            painter.rect_filled(rect, 3.0, Color32::from_white_alpha(210));
            painter.galley(rect.min + Vec2::new(2.0, 0.0), galley, INK);
            drawn += 1;
            if drawn == MAX_NAMES {
                break;
            }
        }
    }

    /// The open pin, at the top of the Search page: its symbol and name, its note, its set
    /// and symbol, the route buttons and Delete. Returns a route button clicked, with its
    /// stop.
    pub fn panel_ui(
        &mut self,
        ui: &mut egui::Ui,
        route: &MenuState,
    ) -> Option<(StopAction, Place)> {
        let r = self.open?;
        let mut buttons = Vec::new();
        let mut close = false;
        let mut delete = false;
        let mut changed = false;
        let (mut typed, mut left) = (false, false);
        let mut move_to = None;
        let names: Vec<String> = self.sets.iter().map(|s| s.name.clone()).collect();
        let focus = std::mem::take(&mut self.focus_name);
        let error = self.sets.get(r.0).and_then(|s| s.error.clone());
        let pin = self.pin_mut(r)?;
        let mut chosen = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                draw_symbol(ui.painter(), rect.center(), HALF, pin.shape, pin.colour);
                let w = (ui.available_width() - 26.0).max(60.0);
                let name = ui.add(
                    egui::TextEdit::singleline(&mut pin.name)
                        .desired_width(w)
                        .font(egui::TextStyle::Body)
                        .hint_text("Name"),
                );
                if focus {
                    name.request_focus();
                    // Typed over: all of it selected.
                    let id = name.id;
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let all = egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(pin.name.chars().count()),
                        );
                        st.cursor.set_char_range(Some(all));
                        st.store(ui.ctx(), id);
                    }
                }
                typed |= name.changed();
                left |= name.lost_focus();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = glyph_button(ui, Glyph::Close, 18.0)
                        .on_hover_text("Close  (Esc)")
                        .clicked();
                });
            });
            let note = ui.add(
                egui::TextEdit::multiline(&mut pin.note)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("Note"),
            );
            typed |= note.changed();
            left |= note.lost_focus();
            ui.horizontal_wrapped(|ui| {
                for shape in Shape::ALL {
                    if symbol_button(ui, shape, pin.colour, pin.shape == shape).clicked() {
                        pin.shape = shape;
                        changed = true;
                    }
                }
                ui.add_space(6.0);
                for colour in Colour::ALL {
                    if symbol_button(ui, pin.shape, colour, pin.colour == colour).clicked() {
                        pin.colour = colour;
                        changed = true;
                    }
                }
            });
            if names.len() > 1 {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Set").size(11.5).color(WEAK));
                    egui::ComboBox::from_id_salt("pin-set")
                        .selected_text(names[r.0].as_str())
                        .show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                if ui.selectable_label(i == r.0, n).clicked() && i != r.0 {
                                    move_to = Some(i);
                                }
                            }
                        });
                });
            } else if let Some(n) = names.first() {
                ui.label(
                    egui::RichText::new(format!("In “{n}”"))
                        .size(11.5)
                        .color(WEAK),
                );
            }
            let [lon, lat] = geo::unit_to_lonlat(geo::to_unit(pin.p));
            ui.label(
                egui::RichText::new(crate::here::lonlat_text(lon, lat))
                    .size(11.5)
                    .color(INK),
            );
            if let Some(e) = &error {
                ui.add(egui::Label::new(egui::RichText::new(e).color(APPROX)).wrap());
            }
            ui.add_space(2.0);
            let (a, _) = routing::stop_buttons(ui, route);
            chosen = a.map(|a| {
                let mut place = Place::at(geo::to_unit(pin.p));
                place.name = Some(pin.name.clone()).filter(|n| !n.is_empty());
                (a, place)
            });
            let d = ui.small_button("Delete pushpin");
            delete = d.clicked();
            buttons.push(("delete", d.rect));
        });
        self.buttons = buttons;
        if typed {
            self.pending = Some(r.0);
            self.version += 1;
        }
        if changed {
            self.touched(r.0);
        }
        if left || close || delete || move_to.is_some() {
            self.flush();
        }
        if let Some(to) = move_to {
            self.open = Some(self.move_to(r, to));
        }
        if delete {
            self.remove(r);
        } else if close {
            self.open = None;
        }
        chosen
    }

    /// Escape closes the open pin; true if it did.
    pub fn escape(&mut self, ctx: &egui::Context) -> bool {
        if self.open.is_none() || ctx.egui_wants_keyboard_input() {
            return false;
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.flush();
            self.open = None;
        }
        esc
    }

    /// The shown pins matching the search box's `text`, above the pack's results, looked
    /// for again only when the text or the pins change. Returns the one clicked.
    pub fn found_ui(&mut self, ui: &mut egui::Ui, text: &str) -> Option<PinRef> {
        let fresh = self
            .found
            .as_ref()
            .is_some_and(|(t, v, _)| t == text && *v == self.version);
        if !fresh {
            self.found = Some((text.to_string(), self.version, self.find(text)));
        }
        let found = self.found.as_ref().map(|f| f.2.clone()).unwrap_or_default();
        if found.is_empty() {
            return None;
        }
        let mut clicked = None;
        ui.label(
            egui::RichText::new("Pushpins")
                .size(11.5)
                .strong()
                .color(WEAK),
        );
        for r in found {
            let Some(p) = self.pin(r) else { continue };
            let resp = pin_row(ui, p, Some(&self.sets[r.0].name));
            if resp.clicked() {
                clicked = Some(r);
            }
        }
        ui.separator();
        clicked
    }
}

/// A row for a pin: its symbol, name and note, and its set if given. Clickable.
pub fn pin_row(ui: &mut egui::Ui, p: &Pin, set: Option<&str>) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let painter = ui.painter_at(rect);
    let c = Pos2::new(rect.left() + 11.0, rect.center().y);
    draw_symbol(&painter, c, 5.5, p.shape, p.colour);
    let mut text = p.name.clone();
    if !p.note.is_empty() {
        text.push_str(" · ");
        text.push_str(p.note.lines().next().unwrap_or_default());
    }
    if let Some(s) = set {
        text.push_str(&format!("  ({s})"));
    }
    painter.text(
        Pos2::new(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        INK,
    );
    resp
}

/// A symbol in the UI, as the map draws it: the shape in its colour, edged in white, with a
/// grey line round it so it shows on the panel.
pub fn draw_symbol(painter: &egui::Painter, c: Pos2, half: f32, shape: Shape, colour: Colour) {
    let fill = colour.color32();
    let edge = Stroke::new(EDGE, Color32::WHITE);
    let outer = Stroke::new(0.75, Color32::from_gray(150));
    let poly = |pts: Vec<Pos2>| {
        painter.add(egui::Shape::convex_polygon(pts.clone(), fill, edge));
        painter.add(egui::Shape::closed_line(pts, outer));
    };
    match shape {
        Shape::Circle => {
            painter.circle(c, half, fill, edge);
            painter.circle_stroke(c, half + 0.5, outer);
        }
        Shape::Square => {
            let h = half * 0.85;
            poly(vec![
                c + Vec2::new(-h, -h),
                c + Vec2::new(h, -h),
                c + Vec2::new(h, h),
                c + Vec2::new(-h, h),
            ]);
        }
        Shape::Diamond => {
            let h = half * 1.15;
            poly(vec![
                c + Vec2::new(0.0, -h),
                c + Vec2::new(h, 0.0),
                c + Vec2::new(0.0, h),
                c + Vec2::new(-h, 0.0),
            ]);
        }
        Shape::Triangle => {
            let h = half * 1.15;
            poly(vec![
                c + Vec2::new(0.0, -h),
                c + Vec2::new(h * 0.95, h * 0.62),
                c + Vec2::new(-h * 0.95, h * 0.62),
            ]);
        }
    }
}

/// A button showing a symbol, framed when it is the one chosen.
fn symbol_button(ui: &mut egui::Ui, shape: Shape, colour: Colour, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    if on {
        ui.painter()
            .rect_stroke(rect, 3.0, Stroke::new(1.5, INK), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    draw_symbol(ui.painter(), rect.center(), 5.5, shape, colour);
    resp
}

/// The bench's pins (`pins = N` in a flight): `n` pins spread over `bounds` (world), the
/// same each run, in one set that is not saved.
pub fn bench_set(n: usize, bounds: [f64; 4]) -> Set {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let pins = (0..n)
        .map(|i| {
            let q = [
                bounds[0] + next() * (bounds[2] - bounds[0]),
                bounds[1] + next() * (bounds[3] - bounds[1]),
            ];
            Pin {
                name: format!("Pin {i}"),
                note: String::new(),
                p: geo::from_unit(q),
                shape: Shape::ALL[i % Shape::ALL.len()],
                colour: Colour::ALL[i % Colour::ALL.len()],
            }
        })
        .collect();
    Set {
        name: "Bench".into(),
        path: PathBuf::new(),
        shown: true,
        pins,
        error: None,
        expanded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_round_trip_and_pins_keep_their_points() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = PinsUi::empty(dir.path().join(DIR));
        let a = geo::project(39.961_176, -82.998_794);
        let r = ui.add("Capitol", a);
        assert_eq!(r, (0, 0));
        assert_eq!(ui.sets[0].name, FIRST_SET);
        {
            let p = ui.pin_mut(r).unwrap();
            p.note = "Tours at 10\nfree".into();
            p.shape = Shape::Diamond;
            p.colour = Colour::Purple;
        }
        ui.save(0);
        // Another set; a new pin goes to the current one, looking like its last.
        let s = ui.new_set("Food");
        assert_eq!(ui.new_set("food"), 2);
        ui.current = s;
        ui.add("Diner", geo::project(41.0, -81.5));
        ui.current = 0;
        let r2 = ui.add("Zoo", geo::project(39.96, -83.13));
        assert_eq!(ui.pin(r2).unwrap().shape, Shape::Diamond);
        ui.set_shown(2, false);

        let back = PinsUi::load(dir.path());
        assert!(back.errors.is_empty(), "{:?}", back.errors);
        let names: Vec<&str> = back.sets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Food", "food 2", FIRST_SET]);
        assert_eq!(back.sets[2].pins, ui.sets[0].pins);
        assert_eq!(
            back.sets[2].pins[0].p, a,
            "the point is kept to the world unit"
        );
        assert!(!back.sets[1].shown && back.sets[0].shown);

        // Search: by name, then by note, in shown sets only.
        assert_eq!(ui.find("ZOO"), [r2]);
        assert_eq!(ui.find("tours"), [(0, 0)]);
        assert_eq!(ui.find("o"), [(0, 0), r2]);
        ui.set_shown(1, false);
        assert_eq!(ui.find("diner"), []);

        // Moving a pin to another set, removing one, and deleting a set.
        let moved = ui.move_to(r2, 1);
        assert_eq!(ui.pin(moved).unwrap().name, "Zoo");
        assert_eq!(ui.sets[0].pins.len(), 1);
        ui.remove(moved);
        ui.rename_set(1, "Lunch").unwrap();
        assert_eq!(files::stem(&ui.sets[1].path), "Lunch");
        ui.delete_set(1).unwrap();
        let names: Vec<String> = files::list(&ui.dir)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["food 2", FIRST_SET]);
    }

    #[test]
    fn a_click_finds_the_nearest_shown_pin() {
        let mut ui = PinsUi::empty(PathBuf::new());
        let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
        cam.size = [1000.0, 700.0];
        cam.zoom = 12.0;
        let at = |cam: &Camera, sx: f64, sy: f64| geo::from_unit(cam.screen_to_world([sx, sy]));
        ui.sets.push(bench_set(0, [0.0; 4]));
        ui.sets[0].pins = vec![
            Pin {
                name: "a".into(),
                note: String::new(),
                p: at(&cam, 500.0, 350.0),
                shape: Shape::Circle,
                colour: Colour::Red,
            },
            Pin {
                name: "b".into(),
                note: String::new(),
                p: at(&cam, 512.0, 350.0),
                shape: Shape::Circle,
                colour: Colour::Red,
            },
        ];
        assert_eq!(ui.hit(&cam, [503.0, 351.0]), Some((0, 0)));
        assert_eq!(ui.hit(&cam, [510.0, 349.0]), Some((0, 1)));
        assert_eq!(ui.hit(&cam, [540.0, 350.0]), None);
        ui.sets[0].shown = false;
        assert_eq!(ui.hit(&cam, [503.0, 351.0]), None);
    }
}
