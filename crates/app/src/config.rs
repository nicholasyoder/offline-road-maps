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
    }
}
