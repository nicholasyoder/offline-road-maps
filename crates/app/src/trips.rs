//! Saved trips (phase 4 step 4.7): one TOML file per trip in `trips/` next to the
//! executable, holding the stops by name and coordinates (never pack IDs, decision 8), the
//! avoid options, the optimizer's ends and the schedule's settings. A stop on a search
//! result's street keeps the street's name, and the routing worker snaps it to a street of
//! that name near its point when the trip is opened, so a trip made on one build of the pack
//! opens on a newer one on the same streets. A leg's route picked among its alternatives is
//! kept as a hidden via point where it leaves the quickest; opening the trip picks the
//! alternative that passes it again.
//!
//! [`TripsUi`] holds the list of saved trips and the windows that save one: a name, and
//! where the folder cannot be written, the file browser.

use std::path::{Path, PathBuf};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::dialog::{self, Size};
use crate::files::{self, Browser, Mode, Outcome, Saved};
use crate::pins::{self, PinRef, PinsUi};
use crate::theme::{Glyph, accent, approx, icon_button, ink, row, weak};

/// The folder next to the executable.
pub const DIR: &str = "trips";
/// The file format's version: a newer one is refused.
pub const VERSION: u32 = 1;

/// A trip as its file holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TripFile {
    pub version: u32,
    /// The pack it was made on, for information only.
    #[serde(default)]
    pub pack: String,
    #[serde(default)]
    pub avoid_tolls: bool,
    #[serde(default)]
    pub avoid_ferries: bool,
    #[serde(default)]
    pub optimize: Optimize,
    #[serde(default)]
    pub schedule: Times,
    #[serde(default, rename = "stop")]
    pub stops: Vec<StopFile>,
}

/// The ends *Optimize stop order* keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Optimize {
    pub keep_first: bool,
    pub keep_last: bool,
    pub round_trip: bool,
}

impl Default for Optimize {
    fn default() -> Self {
        Self {
            keep_first: true,
            keep_last: true,
            round_trip: false,
        }
    }
}

/// *Depart at / Arrive by* and its settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Times {
    pub mode: TimesMode,
    /// `2026-07-11`, local to the stop the time is at.
    pub date: String,
    /// `8:00 AM`.
    pub time: String,
    #[serde(flatten)]
    pub settings: config::Schedule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimesMode {
    #[default]
    Off,
    Depart,
    ArriveBy,
}

/// A stop, or a hidden via point that keeps a leg's route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StopFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// The name of the street it is on, snapped to first when the trip is opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    /// Time spent there.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub minutes: f64,
    #[serde(default)]
    pub kind: StopKind,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    #[default]
    Stop,
    /// A stop that stays the night.
    Overnight,
    /// Not a stop: where the leg's route picked leaves the quickest (step 4.3).
    Via,
}

impl TripFile {
    /// Reads a trip file, refusing one from a newer version of the app.
    pub fn read(path: &Path) -> Result<Self, String> {
        let t: Self = files::read(path)?;
        if t.version > VERSION {
            return Err(format!(
                "{}: saved by a newer version of the app (trip format {})",
                path.display(),
                t.version
            ));
        }
        Ok(t)
    }

    /// The same trip, as compared for unsaved changes: what routing fills in by itself (the
    /// stops' streets' names) and the pack left out, and the points as the app holds them
    /// (to the world unit, about 1 cm), so a file written by hand compares as opened.
    pub fn compared(&self) -> Self {
        let mut t = self.clone();
        t.pack.clear();
        for s in &mut t.stops {
            s.street = None;
            (s.lat, s.lon) = omp_core::geo::unproject(omp_core::geo::project(s.lat, s.lon));
        }
        t
    }

    /// A name for a new trip: "First to Last", from its named stops.
    pub fn suggested_name(&self) -> String {
        let named: Vec<&str> = self
            .stops
            .iter()
            .filter(|s| s.kind != StopKind::Via)
            .filter_map(|s| s.name.as_deref())
            .collect();
        match (named.first(), named.last()) {
            (Some(a), Some(b)) if named.len() > 1 => {
                let short = |s: &str| s.split(',').next().unwrap_or(s).trim().to_string();
                format!("{} to {}", short(a), short(b))
            }
            _ => "Trip".into(),
        }
    }
}

/// What the route page asked of a saved trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAsk {
    /// Save to the trip's own file, or ask for a name the first time.
    Save,
    /// Ask for a name.
    SaveAs,
}

/// The trip a route page shows, once saved or opened: its name and file.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub name: String,
    pub path: PathBuf,
    /// As saved or opened, for unsaved changes ([`TripFile::compared`]).
    pub file: TripFile,
}

/// The save window: a name for the trip, and why it could not be saved.
struct SaveWindow {
    name: String,
    file: TripFile,
    /// A trip of that name exists: Save again replaces it.
    replace: bool,
    error: Option<String>,
    /// The folder cannot be written: offer the file browser.
    elsewhere: bool,
}

/// What the browser was opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Browsing {
    SaveTrip,
    OpenTrip,
    SavePins(usize),
    OpenPins,
}

/// The saved trips and the windows that save and open them.
pub struct TripsUi {
    pub dir: PathBuf,
    /// The folder's trips, read again when it may have changed.
    list: Option<Result<Vec<Saved>, String>>,
    save: Option<SaveWindow>,
    /// The file browser, and what for; its file for a trip to save.
    pub browser: Option<(Browser, Browsing, Option<TripFile>)>,
    /// The trip being renamed, and the name typed.
    pub renaming: Option<(PathBuf, String)>,
    /// The trip (file and name) whose Delete was clicked, asked about in a window.
    pub deleting: Option<(PathBuf, String)>,
    /// What went wrong last, shown on the Trips page.
    pub error: Option<String>,
    /// What the page drew last frame, for the tests to click on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) targets: Vec<(String, egui::Rect)>,
}

/// What the Trips page asked for, done once it is drawn.
#[derive(Default)]
pub struct PageOut {
    /// A trip to open in the route page.
    pub open: Option<PathBuf>,
    /// A trip renamed (from, to): the route page's own follows.
    pub renamed: Option<(PathBuf, PathBuf)>,
    /// A pushpin clicked in a set's list.
    pub show_pin: Option<PinRef>,
    /// *Save trip* or *Save as…* was clicked: the app saves the route shown.
    pub save: Option<SaveAsk>,
}

/// The route shown, as the Trips page's save buttons see it.
pub struct Current<'a> {
    /// It has a start and a destination.
    pub can_save: bool,
    /// Its saved trip, if any.
    pub named: Option<&'a Named>,
    pub unsaved: bool,
}

/// The side of the buttons on a saved item's row, and the space between them.
const ROW_H: f32 = 20.0;
const BUTTON_GAP: f32 = 2.0;

impl TripsUi {
    pub fn new(exe_dir: &Path) -> Self {
        Self {
            dir: exe_dir.join(DIR),
            list: None,
            save: None,
            browser: None,
            renaming: None,
            deleting: None,
            error: None,
            targets: Vec::new(),
        }
    }

    /// The saved trips, read the first time and after a change.
    pub fn list(&mut self) -> &Result<Vec<Saved>, String> {
        let dir = &self.dir;
        self.list.get_or_insert_with(|| {
            files::list(dir).map_err(|e| {
                format!(
                    "The trips in {} cannot be listed: {}.",
                    dir.display(),
                    files::io_text(&e)
                )
            })
        })
    }

    /// Reads the folder again on the next look.
    pub fn changed(&mut self) {
        self.list = None;
    }

    /// Saves `file` to `named`'s file, or opens the save window for a name (`SaveAs`, or a
    /// trip not saved yet, or one that could not be written).
    pub fn save(&mut self, ask: SaveAsk, file: TripFile, named: Option<&Named>) -> Option<Named> {
        if let (SaveAsk::Save, Some(n)) = (ask, named) {
            match files::write(&n.path, &file) {
                Ok(()) => {
                    self.changed();
                    return Some(Named { file, ..n.clone() });
                }
                Err(e) => {
                    let dir = n.path.parent().unwrap_or(&self.dir);
                    self.save = Some(SaveWindow {
                        name: n.name.clone(),
                        error: Some(files::save_error(dir, &e)),
                        elsewhere: true,
                        replace: false,
                        file,
                    });
                    return None;
                }
            }
        }
        let name = named.map_or_else(|| file.suggested_name(), |n| n.name.clone());
        self.save = Some(SaveWindow {
            name,
            file,
            replace: false,
            error: None,
            elsewhere: false,
        });
        None
    }

    /// Writes `file` to `path` (from the browser, or the trips folder).
    fn write(&mut self, path: PathBuf, file: TripFile) -> Result<Named, String> {
        files::write(&path, &file)
            .map_err(|e| files::save_error(path.parent().unwrap_or(&self.dir), &e))?;
        self.changed();
        Ok(Named {
            name: files::stem(&path),
            path,
            file,
        })
    }

    /// Opens the browser to open a trip from anywhere.
    pub fn browse_open(&mut self, exe_dir: &Path) {
        let start = if self.dir.is_dir() {
            &self.dir
        } else {
            exe_dir
        };
        let b = Browser::new("Open a trip", Mode::Open, start, "");
        self.browser = Some((b, Browsing::OpenTrip, None));
    }

    /// The save window. Returns the trip once saved.
    pub fn save_window(&mut self, ctx: &egui::Context, exe_dir: &Path) -> Option<Named> {
        let w = self.save.as_mut()?;
        let mut targets = Vec::new();
        let (mut save, mut cancel, mut elsewhere) = (false, false, false);
        let closed = dialog::show(ctx, "save-trip", "Save trip", Size::Fit(360.0), |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                let r = ui.add(egui::TextEdit::singleline(&mut w.name).desired_width(240.0));
                if r.changed() {
                    w.replace = false;
                }
                save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            if w.replace {
                ui.label(
                    egui::RichText::new(format!(
                        "A trip named “{}” exists. Save again to replace it.",
                        files::file_name(&w.name)
                    ))
                    .color(approx()),
                );
            }
            if let Some(e) = &w.error {
                ui.add(egui::Label::new(egui::RichText::new(e).color(approx())).wrap());
            }
            ui.horizontal(|ui| {
                let label = if w.replace { "Replace" } else { "Save" };
                let r = ui.add_enabled(!w.name.trim().is_empty(), egui::Button::new(label));
                targets.push(("save-window".to_string(), r.rect));
                save |= r.clicked();
                cancel = ui.button("Cancel").clicked();
                elsewhere = ui
                    .button("Save elsewhere…")
                    .on_hover_text("Choose a folder and file in the file browser")
                    .clicked();
            });
        });
        self.targets.retain(|t| t.0 != "save-window");
        self.targets.extend(targets);
        if closed || cancel {
            self.save = None;
            return None;
        }
        if elsewhere {
            let w = self.save.take()?;
            let start = if self.dir.is_dir() {
                &self.dir
            } else {
                exe_dir
            };
            let b = Browser::new("Save trip", Mode::Save, start, &files::file_name(&w.name));
            self.browser = Some((b, Browsing::SaveTrip, Some(w.file)));
            return None;
        }
        if !save || w.name.trim().is_empty() {
            return None;
        }
        let path = files::path_for(&self.dir, w.name.trim());
        if path.exists() && !w.replace {
            w.replace = true;
            return None;
        }
        let file = w.file.clone();
        match self.write(path, file) {
            Ok(n) => {
                self.save = None;
                Some(n)
            }
            Err(e) => {
                let w = self.save.as_mut()?;
                w.error = Some(e);
                w.elsewhere = true;
                None
            }
        }
    }

    /// The browser's window, for a trip to save or open. Returns the trip saved, or the
    /// file of a trip to open; a pushpin set's file is left to [`crate::pins`].
    pub fn browser_window(&mut self, ctx: &egui::Context) -> Option<Browsed> {
        let (b, what, _) = self.browser.as_mut()?;
        let what = *what;
        let outcome = b.ui(ctx)?;
        let (_, _, file) = self.browser.take()?;
        let Outcome::Chosen(path) = outcome else {
            return None;
        };
        match (what, file) {
            (Browsing::SaveTrip, Some(file)) => match self.write(path, file) {
                Ok(n) => Some(Browsed::Saved(Box::new(n))),
                Err(e) => {
                    self.error = Some(e);
                    None
                }
            },
            (Browsing::OpenTrip, _) => Some(Browsed::OpenTrip(path)),
            (Browsing::SavePins(set), _) => Some(Browsed::SavePins(set, path)),
            (Browsing::OpenPins, _) => Some(Browsed::OpenPins(path)),
            (Browsing::SaveTrip, None) => None,
        }
    }
}

impl TripsUi {
    /// The Trips page: the saved trips (open, rename, duplicate, delete, or open one from a
    /// file), then the pushpin sets (show or hide, rename, delete, and a set's pins).
    /// `can_open` is false while no pack
    /// with routing is open.
    pub fn page_ui(
        &mut self,
        ui: &mut egui::Ui,
        pins: &mut PinsUi,
        current: Current<'_>,
        can_open: bool,
        exe_dir: &Path,
    ) -> PageOut {
        self.targets.clear();
        let mut out = PageOut::default();
        egui::ScrollArea::vertical()
            .id_salt("trips-page")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                self.trips_ui(ui, &current, can_open, exe_dir, &mut out);
                ui.separator();
                self.pins_ui(ui, pins, exe_dir, &mut out);
            });
        out
    }

    /// A heading with buttons on its right, in order: each `(id, label, enabled, tip, why
    /// not)`. Returns whether each was clicked.
    fn heading(
        &mut self,
        ui: &mut egui::Ui,
        text: &str,
        buttons: &[(&str, &str, bool, String, &str)],
    ) -> Vec<bool> {
        let mut clicked = vec![false; buttons.len()];
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(text).strong().color(ink()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (i, (id, label, enabled, tip, why)) in buttons.iter().enumerate().rev() {
                    let r = ui
                        .add_enabled(*enabled, egui::Button::new(*label).small())
                        .on_hover_text(tip)
                        .on_disabled_hover_text(*why);
                    self.targets.push((id.to_string(), r.rect));
                    clicked[i] = r.clicked();
                }
            });
        });
        clicked
    }

    /// The line saying what the route shown is, under the save buttons.
    fn save_note(ui: &mut egui::Ui, current: &Current<'_>) {
        let (text, color) = match (current.can_save, current.named, current.unsaved) {
            (false, _, _) => (
                "Plan a route on the Route page to save it as a trip.".into(),
                weak(),
            ),
            (true, Some(n), false) => {
                (format!("The route shown is the trip “{}”.", n.name), weak())
            }
            (true, Some(n), true) => (
                format!(
                    "The route shown is the trip “{}”, with unsaved changes.",
                    n.name
                ),
                accent(),
            ),
            (true, None, _) => ("The route shown is not saved.".into(), weak()),
        };
        ui.label(egui::RichText::new(text).size(11.5).color(color));
        ui.add_space(4.0);
    }

    fn trips_ui(
        &mut self,
        ui: &mut egui::Ui,
        current: &Current<'_>,
        can_open: bool,
        exe_dir: &Path,
        out: &mut PageOut,
    ) {
        let save_tip = match current.named {
            Some(n) => format!("Save to {}", n.path.display()),
            None => "Keep the route shown, with its stops and options, as a trip".into(),
        };
        let save_why = match current.can_save {
            true => "Saved: no changes since",
            false => "Plan a route on the Route page to save it",
        };
        let mut buttons = vec![(
            "save-trip",
            "Save",
            current.can_save && current.unsaved,
            save_tip,
            save_why,
        )];
        if current.named.is_some() {
            buttons.push((
                "save-as",
                "Save as…",
                current.can_save,
                "Save under another name".into(),
                "Plan a route on the Route page to save it",
            ));
        }
        buttons.push((
            "open-file",
            "Open…",
            can_open,
            "Open a trip saved elsewhere".into(),
            "Open a map with routing to open a trip",
        ));
        let clicked = self.heading(ui, "Saved trips", &buttons);
        let n = buttons.len();
        if clicked[0] {
            out.save = Some(SaveAsk::Save);
        }
        if current.named.is_some() && clicked[1] {
            out.save = Some(SaveAsk::SaveAs);
        }
        if clicked[n - 1] {
            self.browse_open(exe_dir);
        }
        Self::save_note(ui, current);
        let list = match self.list() {
            Ok(l) => l.clone(),
            Err(e) => {
                let e = e.clone();
                ui.label(egui::RichText::new(e).color(approx()));
                Vec::new()
            }
        };
        if list.is_empty() {
            ui.label(egui::RichText::new("No saved trips yet.").color(weak()));
        }
        for t in &list {
            match self.renaming.as_mut().filter(|(p, _)| *p == t.path) {
                Some((_, name)) => {
                    let (mut ok, mut cancel) = (false, false);
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(170.0));
                        ok = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        ok |= ui.small_button("Rename").clicked();
                        cancel |= ui.small_button("Cancel").clicked();
                        if !ok && !cancel {
                            r.request_focus();
                        }
                    });
                    if ok {
                        let name = name.trim().to_string();
                        self.renaming = None;
                        match files::rename(&t.path, &name) {
                            Ok(to) => out.renamed = Some((t.path.clone(), to)),
                            Err(e) => {
                                self.error =
                                    Some(format!("Could not rename: {}.", files::io_text(&e)));
                            }
                        }
                        self.changed();
                    } else if cancel {
                        self.renaming = None;
                    }
                }
                None => {
                    let open = current.named.is_some_and(|n| n.path == t.path);
                    self.trip_row(ui, t, open, can_open, out);
                }
            }
        }
        if let Some(e) = &self.error {
            ui.add(egui::Label::new(egui::RichText::new(e).color(approx())).wrap());
        }
    }

    /// A saved trip's row; `open` if it is the route shown.
    fn trip_row(
        &mut self,
        ui: &mut egui::Ui,
        t: &Saved,
        open: bool,
        can_open: bool,
        out: &mut PageOut,
    ) {
        ui.horizontal(|ui| {
            let text = egui::RichText::new(&t.name).color(ink());
            let text = if open { text.strong() } else { text };
            let row = item_row(ui, text, can_open, true, |_| ());
            let o = row.row.on_hover_text(match can_open {
                true => "Open in the Route page",
                false => "Open a map with routing to open a trip",
            });
            let r = row.rename.on_hover_text("Rename");
            let c = row.copy.map(|c| c.on_hover_text("Duplicate"));
            let d = row.delete.on_hover_text("Delete");
            for (k, resp) in [
                ("open", Some(&o)),
                ("rename", Some(&r)),
                ("copy", c.as_ref()),
                ("delete", Some(&d)),
            ] {
                if let Some(resp) = resp {
                    self.targets.push((format!("{k}:{}", t.name), resp.rect));
                }
            }
            if o.clicked() {
                out.open = Some(t.path.clone());
            }
            if r.clicked() {
                self.renaming = Some((t.path.clone(), t.name.clone()));
            }
            if c.is_some_and(|c| c.clicked()) {
                if let Err(e) = files::duplicate(&t.path) {
                    self.error = Some(format!("Could not copy: {}.", files::io_text(&e)));
                }
                self.changed();
            }
            if d.clicked() {
                self.deleting = Some((t.path.clone(), t.name.clone()));
            }
        });
    }

    /// The window that asks before a trip, or a pushpin set, is deleted, and does it on
    /// *Delete*. Enter says yes and Esc no. Drawn with the app's other windows, not inside
    /// the panel. Returns the trip deleted, for the route page's own to follow.
    pub fn delete_window(&mut self, ctx: &egui::Context, pins: &mut PinsUi) -> Option<PathBuf> {
        let mut gone = None;
        let (title, what) = match (&self.deleting, pins.deleting) {
            (Some((_, name)), _) => (
                "Delete trip?",
                format!("The trip “{name}” and its file will be deleted."),
            ),
            (None, Some(i)) => {
                let set = &pins.sets[i];
                let n = set.pins.len();
                (
                    "Delete pushpin set?",
                    format!(
                        "The set “{}” and its file will be deleted, with its {n} pushpin{}.",
                        set.name,
                        if n == 1 { "" } else { "s" }
                    ),
                )
            }
            (None, None) => return None,
        };
        let mut buttons = [egui::Rect::NOTHING; 2];
        let answer = dialog::confirm(ctx, title, &what, "Delete", &mut buttons);
        self.targets.push(("confirm-cancel".into(), buttons[0]));
        self.targets.push(("confirm-delete".into(), buttons[1]));
        match answer {
            Some(false) => {
                self.deleting = None;
                pins.deleting = None;
            }
            Some(true) => {
                if let Some((path, name)) = self.deleting.take() {
                    match std::fs::remove_file(&path) {
                        Ok(()) => gone = Some(path),
                        Err(e) => {
                            self.error =
                                Some(format!("Could not delete {name}: {}.", files::io_text(&e)));
                        }
                    }
                    self.changed();
                } else if let Some(i) = pins.deleting.take()
                    && let Err(e) = pins.delete_set(i)
                {
                    pins.errors.push(e);
                }
            }
            None => {}
        }
        gone
    }

    fn pins_ui(&mut self, ui: &mut egui::Ui, pins: &mut PinsUi, exe_dir: &Path, out: &mut PageOut) {
        let clicked = self.heading(
            ui,
            "Pushpins",
            &[
                (
                    "new-set",
                    "New set",
                    true,
                    "An empty set; new pushpins go to it".into(),
                    "",
                ),
                (
                    "open-set",
                    "Open…",
                    true,
                    "Open a set saved elsewhere".into(),
                    "",
                ),
            ],
        );
        let (new, browse) = (clicked[0], clicked[1]);
        if new {
            pins.current = pins.new_set("Pushpins");
        }
        if browse {
            let start = if pins.dir.is_dir() {
                &pins.dir
            } else {
                exe_dir
            };
            let b = Browser::new("Open a pushpin set", Mode::Open, start, "");
            self.browser = Some((b, Browsing::OpenPins, None));
        }
        if pins.sets.is_empty() {
            ui.label(
                egui::RichText::new("No pushpins. Right-click the map and choose Add pushpin.")
                    .color(weak()),
            );
        }
        let mut act = None;
        for i in 0..pins.sets.len() {
            if let Some((_, name)) = pins.renaming.as_mut().filter(|(k, _)| *k == i) {
                let (mut ok, mut cancel) = (false, false);
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(170.0));
                    ok = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    ok |= ui.small_button("Rename").clicked();
                    cancel |= ui.small_button("Cancel").clicked();
                    if !ok && !cancel {
                        r.request_focus();
                    }
                });
                if ok {
                    act = Some(SetAct::Rename(i, name.trim().to_string()));
                } else if cancel {
                    pins.renaming = None;
                }
                continue;
            }
            let set = &pins.sets[i];
            ui.horizontal(|ui| {
                let fold = if set.expanded { "⏷" } else { "⏵" };
                let text = format!("{fold} {} ({})", set.name, set.pins.len());
                let row = item_row(
                    ui,
                    egui::RichText::new(text).color(ink()),
                    true,
                    false,
                    |ui| {
                        let mut shown = set.shown;
                        let r = ui
                            .checkbox(&mut shown, "")
                            .on_hover_text("Show on the map, and in search");
                        (r, shown)
                    },
                );
                let (check, shown) = row.lead;
                self.targets
                    .push((format!("show:{}", set.name), check.rect));
                if check.changed() {
                    act = Some(SetAct::Shown(i, shown));
                }
                let r = row.row.on_hover_text(match set.expanded {
                    true => "Hide its pushpins",
                    false => "List its pushpins",
                });
                self.targets.push((format!("set:{}", set.name), r.rect));
                if r.clicked() {
                    act = Some(SetAct::Fold(i));
                }
                if row.rename.on_hover_text("Rename").clicked() {
                    act = Some(SetAct::StartRename(i));
                }
                let d = row.delete.on_hover_text("Delete");
                self.targets
                    .push((format!("delete-set:{}", set.name), d.rect));
                if d.clicked() {
                    act = Some(SetAct::Delete(i));
                }
            });
            if let Some(e) = &set.error {
                ui.add(egui::Label::new(egui::RichText::new(e).color(approx())).wrap());
                if ui
                    .small_button("Save elsewhere…")
                    .on_hover_text("Save this set in another folder, as it could not be saved here")
                    .clicked()
                {
                    act = Some(SetAct::Elsewhere(i));
                }
            }
            if set.expanded {
                ui.indent(("set-pins", i), |ui| {
                    if set.pins.is_empty() {
                        ui.label(egui::RichText::new("No pushpins in this set.").color(weak()));
                    }
                    for (k, p) in set.pins.iter().enumerate() {
                        if pins::pin_row(ui, p, None).clicked() {
                            out.show_pin = Some((i, k));
                        }
                    }
                });
            }
        }
        match act {
            Some(SetAct::Shown(i, on)) => pins.set_shown(i, on),
            Some(SetAct::Fold(i)) => pins.sets[i].expanded = !pins.sets[i].expanded,
            Some(SetAct::StartRename(i)) => {
                pins.renaming = Some((i, pins.sets[i].name.clone()));
            }
            Some(SetAct::Rename(i, name)) => {
                pins.renaming = None;
                if let Err(e) = pins.rename_set(i, &name) {
                    pins.errors.push(e);
                }
            }
            Some(SetAct::Delete(i)) => pins.deleting = Some(i),
            Some(SetAct::Elsewhere(i)) => {
                let name = files::file_name(&pins.sets[i].name);
                let b = Browser::new("Save pushpin set", Mode::Save, exe_dir, &name);
                self.browser = Some((b, Browsing::SavePins(i), None));
            }
            None => {}
        }
        for e in &pins.errors {
            ui.add(egui::Label::new(egui::RichText::new(e).color(approx())).wrap());
        }
    }
}

/// A saved item's row, as [`item_row`] draws it.
struct ItemRow<T> {
    /// The row as a whole, which is clicked to open the item.
    row: egui::Response,
    /// What `lead` gave.
    lead: T,
    rename: egui::Response,
    copy: Option<egui::Response>,
    delete: egui::Response,
}

/// A saved item's row across `ui`'s line: what `lead` draws, the name, left aligned and cut
/// to fit, then the icons that rename it, copy it (with `copy`) and delete it. The whole row
/// lights up and is clicked, when `enabled`.
fn item_row<T>(
    ui: &mut egui::Ui,
    text: egui::RichText,
    enabled: bool,
    copy: bool,
    lead: impl FnOnce(&mut egui::Ui) -> T,
) -> ItemRow<T> {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let out = row(ui, sense, |ui| {
        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
        let lead = lead(ui);
        let n = 2 + usize::from(copy);
        let w = (ui.available_width() - n as f32 * (ROW_H + BUTTON_GAP)).max(40.0);
        let text = if enabled { text } else { text.color(weak()) };
        ui.allocate_ui_with_layout(
            egui::vec2(w, ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                // The icons start after the full width, not after the text.
                ui.set_min_width(w);
                ui.add(egui::Label::new(text).truncate().selectable(false))
            },
        );
        let rename = icon_button(ui, Glyph::Rename, ROW_H);
        let copy = copy.then(|| icon_button(ui, Glyph::Copy, ROW_H));
        let delete = icon_button(ui, Glyph::Delete, ROW_H);
        (lead, rename, copy, delete)
    });
    let (lead, rename, copy, delete) = out.inner;
    ItemRow {
        row: out.response,
        lead,
        rename,
        copy,
        delete,
    }
}

/// A change to a pushpin set asked for on the Trips page.
enum SetAct {
    Shown(usize, bool),
    Fold(usize),
    StartRename(usize),
    Rename(usize, String),
    Delete(usize),
    Elsewhere(usize),
}

/// What the browser gave.
pub enum Browsed {
    Saved(Box<Named>),
    OpenTrip(PathBuf),
    SavePins(usize, PathBuf),
    OpenPins(PathBuf),
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> TripFile {
        TripFile {
            version: VERSION,
            pack: "Ohio, built 2026-10-28".into(),
            avoid_tolls: true,
            avoid_ferries: false,
            optimize: Optimize {
                keep_last: false,
                ..Optimize::default()
            },
            schedule: Times {
                mode: TimesMode::ArriveBy,
                date: "2026-07-11".into(),
                time: "17:30".into(),
                settings: config::Schedule {
                    max_drive: true,
                    max_drive_h: 7.5,
                    ..config::Schedule::default()
                },
            },
            stops: vec![
                StopFile {
                    name: Some("Columbus, Ohio".into()),
                    lat: 39.961_176_1,
                    lon: -82.998_794_2,
                    street: Some("North High Street".into()),
                    minutes: 0.0,
                    kind: StopKind::Stop,
                },
                StopFile {
                    name: None,
                    lat: 40.5,
                    lon: -82.0,
                    street: None,
                    minutes: 0.0,
                    kind: StopKind::Via,
                },
                StopFile {
                    name: Some("Mansfield".into()),
                    lat: 40.758_4,
                    lon: -82.515_4,
                    street: None,
                    minutes: 45.0,
                    kind: StopKind::Overnight,
                },
                StopFile {
                    name: Some("Cleveland, Ohio".into()),
                    lat: 41.499_3,
                    lon: -81.694_4,
                    street: None,
                    minutes: 0.0,
                    kind: StopKind::Stop,
                },
            ],
        }
    }

    #[test]
    fn trip_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let t = sample();
        let path = files::path_for(dir.path(), &t.suggested_name());
        assert_eq!(files::stem(&path), "Columbus to Cleveland");
        files::write(&path, &t).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("kind = \"via\"") && text.contains("mode = \"arrive_by\""));
        assert_eq!(TripFile::read(&path).unwrap(), t);
        // A file with only what matters by hand opens with the defaults.
        std::fs::write(&path, "version = 1\n[[stop]]\nlat = 40.0\nlon = -83.0\n").unwrap();
        let t = TripFile::read(&path).unwrap();
        assert_eq!(t.optimize, Optimize::default());
        assert_eq!(t.schedule.mode, TimesMode::Off);
        assert_eq!(t.stops[0].kind, StopKind::Stop);
        // A newer format is refused.
        std::fs::write(&path, "version = 2\n").unwrap();
        assert!(TripFile::read(&path).unwrap_err().contains("newer"));
    }

    #[test]
    fn saving_asks_a_name_the_first_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut ui = TripsUi::new(dir.path());
        let t = sample();
        // Not saved yet: the window opens with a name.
        assert!(ui.save(SaveAsk::Save, t.clone(), None).is_none());
        assert_eq!(ui.save.as_ref().unwrap().name, "Columbus to Cleveland");
        let n = ui
            .write(files::path_for(&ui.dir, "Lake Erie"), t.clone())
            .unwrap();
        assert_eq!(n.name, "Lake Erie");
        assert_eq!(ui.list().as_ref().unwrap().len(), 1);
        // Saved: Save writes it again at once.
        let mut t2 = t.clone();
        t2.avoid_ferries = true;
        let n2 = ui.save(SaveAsk::Save, t2.clone(), Some(&n)).expect("saved");
        assert_eq!(n2.path, n.path);
        assert_eq!(TripFile::read(&n.path).unwrap(), t2);
    }
}
