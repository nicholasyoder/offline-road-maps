//! `config.toml`: the last view, the chosen pack and settings.
//!
//! Portable mode (roadmap §5): the file lives next to the executable, so the app and its
//! settings travel together on a USB stick. If that folder is not writable, the OS config
//! folder is used instead and the app says so.

use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layers::Layers;
use crate::scale::Units;

const FILE: &str = "config.toml";
const APP_DIR: &str = "offline-road-maps";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The pack last opened: its folder, relative to the executable's folder when inside it,
    /// so a stick mounted at another drive letter or path still finds it.
    pub pack: Option<PathBuf>,
    pub view: Option<View>,
    /// The debug overlay (F3).
    pub debug_overlay: bool,
    /// Antialiasing samples: 4 by default; 1 turns MSAA off, for GPUs that cannot do it.
    /// Read when the window is made, so a change applies after a restart.
    pub msaa: Option<u32>,
    /// The scale bar's units; unset, they follow the pack.
    pub units: Option<Units>,
    pub layers: Layers,
    /// The Settings window (phase 5 step 5.3).
    pub display: Display,
    pub movement: Movement,
    pub start: Start,
    /// The last few searches, newest first, offered when the search box is empty.
    pub recent_searches: Vec<String>,
    pub side_panel: SidePanel,
    /// The schedule's settings last used (phase 4 step 4.5), for the next trip.
    pub schedule: Schedule,
    /// The print page's settings (phase 4 step 4.9).
    pub print: crate::print::Settings,
}

/// Display settings: the factors on the UI's and the map labels' sizes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// A factor on egui's zoom factor, which is the system's scale times this.
    pub ui_size: f64,
    /// A factor on the style's label sizes: names, shields and icons on the map only.
    pub label_size: f64,
}

impl Display {
    pub const UI_SIZE: RangeInclusive<f64> = 0.5..=2.0;
    pub const LABEL_SIZE: RangeInclusive<f64> = 0.5..=2.0;
}

impl Default for Display {
    fn default() -> Self {
        Self {
            ui_size: 1.0,
            label_size: 1.0,
        }
    }
}

/// How the map moves under the mouse and keys. The defaults are what the map did before
/// these were settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Movement {
    /// Zoom levels per notch of a mouse wheel.
    pub wheel_zoom: f64,
    /// Zoom levels per press of + or -.
    pub key_zoom: f64,
    /// Points per second an arrow key pans.
    pub key_pan: f64,
    /// A pan thrown by a drag goes on gliding.
    pub inertia: bool,
    /// Seconds a glide takes to slow to a hundredth of its speed.
    pub glide: f64,
}

impl Movement {
    pub const WHEEL_ZOOM: RangeInclusive<f64> = 0.05..=1.0;
    pub const KEY_ZOOM: RangeInclusive<f64> = 0.25..=2.0;
    pub const KEY_PAN: RangeInclusive<f64> = 200.0..=2000.0;
    pub const GLIDE: RangeInclusive<f64> = 0.2..=3.0;

    /// The factor a glide's speed keeps per second.
    pub fn friction(&self) -> f64 {
        0.01_f64.powf(1.0 / self.glide)
    }
}

impl Default for Movement {
    fn default() -> Self {
        Self {
            wheel_zoom: 0.25,
            key_zoom: 1.0,
            key_pan: 700.0,
            inertia: true,
            glide: 0.8,
        }
    }
}

/// What the app opens to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Start {
    /// The last view of a pack, else the pack fitted to the window.
    pub reopen_view: bool,
    /// The pack opened last, when `maps/` holds several or none.
    pub reopen_pack: bool,
}

impl Default for Start {
    fn default() -> Self {
        Self {
            reopen_view: true,
            reopen_pack: true,
        }
    }
}

/// `v` within `range`; a value that is not a number becomes `default`.
fn within(v: f64, range: &RangeInclusive<f64>, default: f64) -> f64 {
    if v.is_finite() {
        v.clamp(*range.start(), *range.end())
    } else {
        default
    }
}

impl Config {
    /// Every value brought into the range its control allows, so a file edited by hand, or
    /// written by another version, cannot hang or break the app (audit R6).
    pub fn clamp(&mut self) {
        let (d, m) = (Display::default(), Movement::default());
        let s = Schedule::default();
        self.display.ui_size = within(self.display.ui_size, &Display::UI_SIZE, d.ui_size);
        self.display.label_size =
            within(self.display.label_size, &Display::LABEL_SIZE, d.label_size);
        let mv = &mut self.movement;
        mv.wheel_zoom = within(mv.wheel_zoom, &Movement::WHEEL_ZOOM, m.wheel_zoom);
        mv.key_zoom = within(mv.key_zoom, &Movement::KEY_ZOOM, m.key_zoom);
        mv.key_pan = within(mv.key_pan, &Movement::KEY_PAN, m.key_pan);
        mv.glide = within(mv.glide, &Movement::GLIDE, m.glide);
        if !matches!(self.msaa, None | Some(1 | 2 | 4)) {
            self.msaa = None;
        }
        let sc = &mut self.schedule;
        sc.max_drive_h = within(sc.max_drive_h, &(1.0..=24.0), s.max_drive_h);
        sc.break_every_h = within(sc.break_every_h, &(0.5..=12.0), s.break_every_h);
        sc.break_min = within(sc.break_min, &(5.0..=240.0), s.break_min);
        self.print.copies = self.print.copies.clamp(1, 99);
        if let Some(v) = &self.view
            && !(v.lat.is_finite() && v.lon.is_finite() && v.zoom.is_finite())
        {
            self.view = None;
        }
    }

    /// The config in `text`, a value at a time: one that does not read (a wrong type, a
    /// word that is no choice) is dropped, with a line saying which, and the rest is kept
    /// (audit R5). Err when `text` is not TOML at all.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), toml::de::Error> {
        let mut table: toml::Table = text.parse()?;
        let mut dropped = Vec::new();
        if let Err(e) = toml::Table::try_into::<Config>(table.clone()) {
            let mut path = Vec::new();
            prune(&mut table, &mut path, &mut dropped);
            if dropped.is_empty() {
                // Nothing narrows it down.
                return Err(e);
            }
        }
        let mut cfg: Config = table.try_into()?;
        cfg.clamp();
        Ok((cfg, dropped))
    }
}

/// Removes from `table` what a [`Config`] cannot be read with, a value at a time: a value
/// that reads as it is stays; a table that does not is looked into; any other is dropped.
fn prune(table: &mut toml::Table, path: &mut Vec<String>, dropped: &mut Vec<String>) {
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        path.push(key.clone());
        if !reads(path, &table[&key]) {
            match table.get_mut(&key) {
                Some(toml::Value::Table(t)) => {
                    let n = dropped.len();
                    prune(t, path, dropped);
                    // What is left of a table that still does not read (a view with no
                    // longitude) goes whole.
                    if !reads(path, &table[&key]) {
                        table.remove(&key);
                        dropped.truncate(n);
                        dropped.push(path.join("."));
                    }
                }
                _ => {
                    table.remove(&key);
                    dropped.push(path.join("."));
                }
            }
        }
        path.pop();
    }
}

/// Whether a config holding only `value` at `path` reads.
fn reads(path: &[String], value: &toml::Value) -> bool {
    let mut v = value.clone();
    for k in path.iter().rev() {
        v = toml::Value::Table(toml::Table::from_iter([(k.clone(), v)]));
    }
    match v {
        toml::Value::Table(t) => t.try_into::<Config>().is_ok(),
        _ => false,
    }
}

/// The schedule's settings, as the route page last had them: times are `HH:MM`, local.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    /// The time *Depart at* starts at when it is turned on.
    pub depart: String,
    /// Drive only between `day_start` and `day_end`.
    pub window: bool,
    pub day_start: String,
    pub day_end: String,
    /// Drive at most `max_drive_h` hours a day.
    pub max_drive: bool,
    pub max_drive_h: f64,
    /// A break of `break_min` minutes every `break_every_h` hours of driving.
    pub breaks: bool,
    pub break_every_h: f64,
    pub break_min: f64,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            depart: "08:00".into(),
            window: true,
            day_start: "08:00".into(),
            day_end: "18:00".into(),
            max_drive: false,
            max_drive_h: 9.0,
            breaks: false,
            break_every_h: 2.0,
            break_min: 15.0,
        }
    }
}

/// The side panel (phase 3 step 3.8): open or closed, and its width in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidePanel {
    pub open: bool,
    pub width: f32,
}

impl Default for SidePanel {
    fn default() -> Self {
        Self {
            open: true,
            width: 360.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
}

/// Where the config is read from and written to.
pub struct Location {
    pub path: PathBuf,
    /// `None` in portable mode; otherwise why the OS config folder is used.
    pub fallback: Option<String>,
}

impl Location {
    /// `config.toml` in `exe_dir` if that folder is writable, else in the OS config folder.
    pub fn find(exe_dir: &Path) -> Self {
        let path = exe_dir.join(FILE);
        let why = match writable(exe_dir, &path) {
            Ok(()) => {
                return Self {
                    path,
                    fallback: None,
                };
            }
            Err(e) => e,
        };
        match os_config_dir() {
            Some(dir) => Self {
                fallback: Some(format!(
                    "{} is not writable ({why}); settings are kept in {}",
                    exe_dir.display(),
                    dir.display()
                )),
                path: dir.join(FILE),
            },
            None => Self {
                fallback: Some(format!(
                    "{} is not writable ({why}) and there is no OS config folder; settings will not be saved",
                    exe_dir.display()
                )),
                path,
            },
        }
    }

    /// The saved config, or the default if there is none, and what to tell the user. A value
    /// that does not read is dropped and the rest kept; a file that is not TOML at all is
    /// renamed `config.toml.bad`, not overwritten, and the app starts with the defaults.
    pub fn load(&self) -> (Config, Option<String>) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) => {
                let note = format!("{}: {e}", self.path.display());
                return (Config::default(), Some(note));
            }
        };
        match Config::parse(&text) {
            Ok((cfg, dropped)) if dropped.is_empty() => (cfg, None),
            Ok((cfg, dropped)) => {
                let note = format!(
                    "{}: could not read {}; those settings are back to their defaults.",
                    self.path.display(),
                    dropped.join(", ")
                );
                (cfg, Some(note))
            }
            Err(e) => {
                let mut bad = self.path.as_os_str().to_os_string();
                bad.push(".bad");
                let kept = match std::fs::rename(&self.path, &bad) {
                    Ok(()) => format!("kept as {}", PathBuf::from(bad).display()),
                    Err(e) => format!("could not be kept ({e})"),
                };
                let note = format!(
                    "{} could not be read ({}) and {kept}; starting with the default settings.",
                    self.path.display(),
                    e.message()
                );
                (Config::default(), Some(note))
            }
        }
    }

    /// Writes through a temporary file, so a crash mid-write leaves the old config.
    pub fn save(&self, cfg: &Config) -> io::Result<()> {
        crate::files::write(&self.path, cfg)
    }
}

fn writable(dir: &Path, path: &Path) -> io::Result<()> {
    if path.exists() {
        return std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map(drop);
    }
    let probe = dir.join(".config.toml.probe");
    std::fs::File::create(&probe)?.write_all(b"")?;
    std::fs::remove_file(probe)
}

fn os_config_dir() -> Option<PathBuf> {
    let env = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join(APP_DIR))
}

/// `pack` as stored: relative to `exe_dir` when it is inside it.
pub fn store_path(pack: &Path, exe_dir: &Path) -> PathBuf {
    pack.strip_prefix(exe_dir)
        .map_or_else(|_| pack.to_path_buf(), Path::to_path_buf)
}

/// A stored pack path made usable again.
pub fn resolve_path(stored: &Path, exe_dir: &Path) -> PathBuf {
    if stored.is_absolute() {
        stored.to_path_buf()
    } else {
        exe_dir.join(stored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_next_to_exe() {
        let dir = tempfile::tempdir().unwrap();
        let loc = Location::find(dir.path());
        assert!(loc.fallback.is_none());
        assert_eq!(loc.load(), (Config::default(), None));
        let cfg = Config {
            pack: Some(store_path(&dir.path().join("maps/ohio"), dir.path())),
            view: Some(View {
                lat: 40.0,
                lon: -83.0,
                zoom: 9.25,
            }),
            debug_overlay: true,
            msaa: Some(2),
            units: Some(Units::Kilometres),
            display: Display {
                ui_size: 0.85,
                label_size: 1.25,
            },
            movement: Movement {
                wheel_zoom: 0.5,
                key_zoom: 0.5,
                key_pan: 400.0,
                inertia: false,
                glide: 1.5,
            },
            start: Start {
                reopen_view: false,
                reopen_pack: false,
            },
            layers: Layers {
                rail: false,
                food: false,
                ..Layers::default()
            },
            recent_searches: vec!["123 Main St, Columbus".into(), "45201".into()],
            side_panel: SidePanel {
                open: false,
                width: 412.0,
            },
            schedule: Schedule {
                window: false,
                max_drive: true,
                max_drive_h: 8.5,
                ..Schedule::default()
            },
            print: crate::print::Settings {
                paper: crate::print::Paper::A4,
                copies: 2,
                ..Default::default()
            },
        };
        assert_eq!(cfg.pack.as_deref(), Some(Path::new("maps/ohio")));
        loc.save(&cfg).unwrap();
        assert_eq!(loc.load(), (cfg, None));
        assert!(!dir.path().join("config.toml.tmp").exists());
    }

    #[test]
    fn damaged_file_is_kept_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, "view = [").unwrap();
        let (cfg, note) = Location::find(dir.path()).load();
        assert_eq!(cfg, Config::default());
        assert!(note.unwrap().contains("config.toml.bad"));
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.bad")).unwrap(),
            "view = ["
        );
    }

    #[test]
    fn one_bad_value_loses_only_itself() {
        let text = "debug_overlay = true\nunits = \"furlongs\"\nmsaa = 2\n\
                    [layers]\nrail = false\nfood = 3\n\
                    [movement]\nkey_pan = \"fast\"\nwheel_zoom = 0.5\n\
                    [view]\nlat = 40.0\n";
        let (c, dropped) = Config::parse(text).unwrap();
        assert!(c.debug_overlay && c.units.is_none() && c.msaa == Some(2));
        assert!(!c.layers.rail && c.layers.food);
        assert_eq!(c.movement.key_pan, Movement::default().key_pan);
        assert_eq!(c.movement.wheel_zoom, 0.5);
        assert!(c.view.is_none());
        assert_eq!(
            dropped,
            ["layers.food", "movement.key_pan", "units", "view"]
        );
    }

    #[test]
    fn values_are_clamped_to_their_controls() {
        let text = "msaa = 3\n[display]\nui_size = 40.0\nlabel_size = nan\n\
                    [movement]\nglide = 0.0\nkey_pan = -5.0\n\
                    [schedule]\nmax_drive_h = 0.0\nbreak_every_h = -2.0\nbreak_min = 1e9\n\
                    [print]\ncopies = 0\n";
        let (c, dropped) = Config::parse(text).unwrap();
        assert!(dropped.is_empty());
        assert_eq!(c.msaa, None);
        assert_eq!(c.display.ui_size, 2.0);
        assert_eq!(c.display.label_size, 1.0);
        assert_eq!(c.movement.glide, 0.2);
        assert_eq!(c.movement.key_pan, 200.0);
        assert_eq!(c.schedule.max_drive_h, 1.0);
        assert_eq!(c.schedule.break_every_h, 0.5);
        assert_eq!(c.schedule.break_min, 240.0);
        assert_eq!(c.print.copies, 1);
    }

    #[test]
    fn the_default_glide_is_the_old_friction() {
        let f = Movement::default().friction();
        assert!((f - 0.004).abs() < 0.001, "{f}");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let c: Config = toml::from_str("debug_overlay = true\nfuture = 1\n").unwrap();
        assert!(c.debug_overlay);
        // A config from before a toggle existed shows that layer.
        let c: Config = toml::from_str("[layers]\nrail = false\n").unwrap();
        assert!(!c.layers.rail && c.layers.labels && c.layers.fuel);
        assert_eq!(c.side_panel, SidePanel::default());
    }
}
