//! `config.toml`: the last view, the chosen pack and settings.
//!
//! Portable mode (roadmap §5): the file lives next to the executable, so the app and its
//! settings travel together on a USB stick. If that folder is not writable, the OS config
//! folder is used instead and the app says so.

use std::io::{self, Write};
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
    pub msaa: Option<u32>,
    /// The scale bar's units; unset, they follow the pack.
    pub units: Option<Units>,
    pub layers: Layers,
    /// The last few searches, newest first, offered when the search box is empty.
    pub recent_searches: Vec<String>,
    pub side_panel: SidePanel,
    /// The schedule's settings last used (phase 4 step 4.5), for the next trip.
    pub schedule: Schedule,
    /// The print page's settings (phase 4 step 4.9).
    pub print: crate::print::Settings,
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

    /// The saved config, or the default if there is none. A damaged file is reported and
    /// replaced on the next save.
    pub fn load(&self) -> (Config, Option<String>) {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => (c, None),
                Err(e) => (
                    Config::default(),
                    Some(format!("{}: {e}", self.path.display())),
                ),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (
                Config::default(),
                Some(format!("{}: {e}", self.path.display())),
            ),
        }
    }

    /// Writes through a temporary file, so a crash mid-write leaves the old config.
    pub fn save(&self, cfg: &Config) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(cfg).map_err(io::Error::other)?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
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
            msaa: Some(1),
            units: Some(Units::Kilometres),
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
    fn damaged_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "view = 3").unwrap();
        let (cfg, err) = Location::find(dir.path()).load();
        assert_eq!(cfg, Config::default());
        assert!(err.is_some());
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
