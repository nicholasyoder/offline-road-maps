//! Files next to the executable (phase 4 step 4.7): the `trips/` and `pins/` folders, one
//! TOML file per trip or pushpin set, named after it; and the in-app file browser, used
//! where those folders cannot be written (a locked stick) and to open a file from
//! elsewhere. No native dialog (decision 9): `rfd` would pull in GTK or a D-Bus portal on
//! Linux. Steps 4.8 and 4.10 add their formats to the same browser.

use std::io;
use std::path::{Path, PathBuf};

use eframe::egui;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::theme::{APPROX, INK, WEAK};

/// The extension of trips and pushpin sets.
pub const EXT: &str = "toml";

/// A file in one of the folders: its name as shown (the file's stem) and its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub path: PathBuf,
}

/// The `.toml` files in `dir`, by name, case aside; none if it does not exist.
pub fn list(dir: &Path) -> io::Result<Vec<Saved>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for e in entries {
        let path = e?.path();
        if path.extension().is_some_and(|x| x == EXT) && path.is_file() {
            let name = stem(&path);
            out.push(Saved { name, path });
        }
    }
    out.sort_by_key(|s| (s.name.to_lowercase(), s.name.clone()));
    Ok(out)
}

/// The name a file is shown by: its stem.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `name` made safe as a file name on Windows and Linux: no path separators or characters
/// Windows refuses, and no dots or spaces at the ends.
pub fn file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let s = s.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if s.is_empty() {
        "Untitled".into()
    } else {
        s.into()
    }
}

/// Where a file named `name` goes in `dir`.
pub fn path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.{EXT}", file_name(name)))
}

/// Reads a TOML file, saying which file a fault is in.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes `value` as TOML through a temporary file, so a crash mid-write leaves the old file,
/// making the folder if need be.
pub fn write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(value).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("{EXT}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Renames the file at `path` to `name` in the same folder; refuses to replace another.
pub fn rename(path: &Path, name: &str) -> io::Result<PathBuf> {
    let to = path_for(path.parent().unwrap_or(Path::new(".")), name);
    if to == path {
        return Ok(to);
    }
    // A change of case only: the same file on Windows.
    let same = to.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
    if to.exists() && !same {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("“{}” already exists", stem(&to)),
        ));
    }
    std::fs::rename(path, &to)?;
    Ok(to)
}

/// Copies the file at `path` as "Name copy" (or "Name copy 2", …) in the same folder.
pub fn duplicate(path: &Path) -> io::Result<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = stem(path);
    let to = (1..1000)
        .map(|k| match k {
            1 => format!("{name} copy"),
            k => format!("{name} copy {k}"),
        })
        .map(|n| path_for(dir, &n))
        .find(|p| !p.exists())
        .ok_or_else(|| io::Error::other("too many copies"))?;
    std::fs::copy(path, &to)?;
    Ok(to)
}

/// Why a file could not be saved in `dir`, said to the user.
pub fn save_error(dir: &Path, e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => format!(
            "{} cannot be written (it may be on a locked stick). Save somewhere else?",
            dir.display()
        ),
        _ => format!("Could not save in {}: {e}", dir.display()),
    }
}

/// Whether the file browser saves a file or opens one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Save,
    Open,
}

/// What the browser's window gave this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Chosen(PathBuf),
    Cancelled,
}

/// The in-app file browser: a window with the folder's subfolders and its files of one
/// extension, a path field to go anywhere, and a file name to save as.
pub struct Browser {
    pub title: String,
    pub mode: Mode,
    pub dir: PathBuf,
    /// The file name typed, without its extension.
    pub name: String,
    /// The folder's subfolders and files, read when it changes.
    entries: Option<io::Result<(Vec<String>, Vec<String>)>>,
    /// The folder field, as typed.
    path_field: String,
    /// Saving over an existing file was asked: it needs a second click.
    replace: Option<PathBuf>,
}

impl Browser {
    pub fn new(title: &str, mode: Mode, dir: &Path, name: &str) -> Self {
        Self {
            title: title.into(),
            mode,
            dir: dir.to_path_buf(),
            name: name.into(),
            entries: None,
            path_field: dir.display().to_string(),
            replace: None,
        }
    }

    fn go(&mut self, dir: PathBuf) {
        self.path_field = dir.display().to_string();
        self.dir = dir;
        self.entries = None;
        self.replace = None;
    }

    fn read(&mut self) -> &io::Result<(Vec<String>, Vec<String>)> {
        self.entries.get_or_insert_with(|| {
            let (mut dirs, mut files) = (Vec::new(), Vec::new());
            for e in std::fs::read_dir(&self.dir)? {
                let Ok(e) = e else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if path.is_dir() {
                    dirs.push(name);
                } else if path.extension().is_some_and(|x| x == EXT) {
                    files.push(name);
                }
            }
            let key = |s: &String| s.to_lowercase();
            dirs.sort_by_key(key);
            files.sort_by_key(key);
            Ok((dirs, files))
        })
    }

    /// The file a click on Save or Open picks.
    fn target(&self) -> Option<PathBuf> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        let path = match self.mode {
            Mode::Save => path_for(&self.dir, name),
            Mode::Open => self.dir.join(name),
        };
        Some(path)
    }

    /// Draws the window. Returns the file picked, or that the window was closed.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        let mut out = None;
        let mut open = true;
        egui::Window::new(self.title.clone())
            .id(egui::Id::unique("file-browser"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 360.0])
            .show(ctx, |ui| out = self.body(ui));
        if !open {
            out = Some(Outcome::Cancelled);
        }
        out
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<Outcome> {
        let mut out = None;
        ui.horizontal(|ui| {
            let up = self.dir.parent().map(Path::to_path_buf);
            if ui
                .add_enabled(up.is_some(), egui::Button::new("⬆ Up"))
                .clicked()
                && let Some(up) = up
            {
                self.go(up);
            }
            let r = ui
                .add(egui::TextEdit::singleline(&mut self.path_field).desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let p = PathBuf::from(self.path_field.trim());
                if p.is_dir() {
                    self.go(p);
                }
            }
        });
        ui.separator();
        let mut go = None;
        let mut picked = None;
        let list_h = (ui.available_height() - 64.0).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| match self.read() {
                Err(e) => {
                    ui.label(
                        egui::RichText::new(format!("Cannot read this folder: {e}")).color(APPROX),
                    );
                }
                Ok((dirs, files)) => {
                    for d in dirs {
                        if ui.selectable_label(false, format!("🗀 {d}")).clicked() {
                            go = Some(d.clone());
                        }
                    }
                    for f in files {
                        let r = ui.selectable_label(false, egui::RichText::new(f).color(INK));
                        if r.clicked() {
                            picked = Some((f.clone(), false));
                        }
                        if r.double_clicked() {
                            picked = Some((f.clone(), true));
                        }
                    }
                    if dirs.is_empty() && files.is_empty() {
                        ui.label(egui::RichText::new("Nothing here.").color(WEAK));
                    }
                }
            });
        if let Some(d) = go {
            let dir = self.dir.join(d);
            self.go(dir);
        }
        let mut submit = false;
        if let Some((f, twice)) = picked {
            self.name = match self.mode {
                Mode::Save => f.strip_suffix(&format!(".{EXT}")).unwrap_or(&f).to_string(),
                Mode::Open => f,
            };
            self.replace = None;
            submit = twice;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(240.0));
            if r.changed() {
                self.replace = None;
            }
            if self.mode == Mode::Save {
                ui.label(egui::RichText::new(format!(".{EXT}")).color(WEAK));
            }
            submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        ui.horizontal(|ui| {
            let label = match (self.mode, &self.replace) {
                (Mode::Save, Some(_)) => "Replace",
                (Mode::Save, None) => "Save",
                (Mode::Open, _) => "Open",
            };
            let target = self.target();
            submit |= ui
                .add_enabled(target.is_some(), egui::Button::new(label))
                .clicked();
            if ui.button("Cancel").clicked() {
                out = Some(Outcome::Cancelled);
            }
            if let Some(p) = &self.replace {
                ui.label(egui::RichText::new(format!("“{}” exists.", stem(p))).color(APPROX));
            }
            if submit && let Some(t) = target {
                out = match self.mode {
                    Mode::Save if t.exists() && self.replace.as_ref() != Some(&t) => {
                        self.replace = Some(t);
                        None
                    }
                    Mode::Open if !t.is_file() => None,
                    _ => Some(Outcome::Chosen(t)),
                };
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_copies() {
        assert_eq!(file_name("Ohio: summer / fall?"), "Ohio- summer - fall-");
        assert_eq!(file_name(" .. "), "Untitled");
        let dir = tempfile::tempdir().unwrap();
        let a = path_for(dir.path(), "Trip");
        write(&a, &toml::toml! { x = 1 }).unwrap();
        assert!(!dir.path().join("Trip.toml.tmp").exists());
        let b = duplicate(&a).unwrap();
        let c = duplicate(&a).unwrap();
        assert_eq!(
            (stem(&b), stem(&c)),
            ("Trip copy".into(), "Trip copy 2".into())
        );
        let names: Vec<String> = list(dir.path())
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["Trip", "Trip copy", "Trip copy 2"]);
        // Renaming onto another file is refused; onto a new name it moves.
        assert!(rename(&b, "Trip").is_err());
        let d = rename(&b, "Lake trip").unwrap();
        assert_eq!(stem(&d), "Lake trip");
        assert!(!b.exists() && d.exists());
        assert!(list(&dir.path().join("none")).unwrap().is_empty());
    }
}
