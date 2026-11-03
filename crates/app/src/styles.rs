//! The styles the Map tab lists (phase 5 step 5.4): the built-in ones, `style.toml` next to
//! the executable, and every `styles/*.toml` there. A style is chosen by its id, which the
//! config keeps: `default`, `night`, `high-contrast`, `custom` for `style.toml`, and
//! `custom:<file name>` for the others.

use std::path::{Path, PathBuf};

use crate::style::{self, Rgba, StyleFile};

/// The folder of extra styles next to the executable.
pub const DIR: &str = "styles";

/// The id of the style used when the config names none, or one that is gone.
pub const DEFAULT: &str = "default";

const BUILTIN: [(&str, &str, &str); 3] = [
    ("default", "Default", style::DEFAULT),
    ("night", "Night", include_str!("../data/night.toml")),
    (
        "high-contrast",
        "High contrast",
        include_str!("../data/high-contrast.toml"),
    ),
];

pub struct Entry {
    pub id: String,
    pub name: String,
    /// The file it comes from, if it is not built in: reloaded in debug builds.
    pub path: Option<PathBuf>,
    /// The style, or why it cannot be used.
    pub file: Result<StyleFile, String>,
}

impl Entry {
    pub fn swatch(&self) -> Option<[Rgba; 3]> {
        self.file.as_ref().ok().map(StyleFile::swatch)
    }
}

/// The built-in styles, then `style.toml` and `styles/*.toml` next to the executable, by
/// name, then `forced` (`--style`) if given.
pub fn list(exe_dir: &Path, forced: Option<&Path>) -> Vec<Entry> {
    let mut v: Vec<Entry> = BUILTIN
        .iter()
        .map(|&(id, name, text)| Entry {
            id: id.into(),
            name: name.into(),
            path: None,
            file: StyleFile::parse(text),
        })
        .collect();
    let mut custom = Vec::new();
    let one = exe_dir.join(style::FILE);
    if one.is_file() {
        custom.push(("custom".to_string(), "Custom (style.toml)".to_string(), one));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(exe_dir.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
        .collect();
    files.sort();
    for p in files {
        let file = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into());
        let stem = p
            .file_stem()
            .map_or_else(String::new, |n| n.to_string_lossy().into());
        custom.push((format!("custom:{file}"), stem, p));
    }
    for (id, name, path) in custom {
        v.push(from_file(id, name, path));
    }
    if let Some(p) = forced {
        v.push(from_file(
            FORCED.into(),
            format!("{} (--style)", p.display()),
            p.to_path_buf(),
        ));
    }
    v
}

/// The id of the style `--style` names.
pub const FORCED: &str = "--style";

/// A style read from `path`. The files in `styles/` are named by their own `name` when
/// they have one, else `fallback`; `style.toml` and `--style` always `fallback`.
fn from_file(id: String, fallback: String, path: PathBuf) -> Entry {
    let file = StyleFile::read(&path);
    let own = id.starts_with("custom:");
    let name = match &file {
        Ok(f) if own => f.name.clone().unwrap_or(fallback),
        _ => fallback,
    };
    Entry {
        id,
        name,
        path: Some(path),
        file,
    }
}

/// The entry with `id`.
pub fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_styles_parse_and_differ() {
        let v = list(Path::new("/nonexistent"), None);
        assert_eq!(v.len(), 3);
        for e in &v {
            assert!(e.file.is_ok(), "{}: {:?}", e.id, e.file.as_ref().err());
        }
        let sw: Vec<_> = v.iter().map(|e| e.swatch().unwrap()).collect();
        assert_ne!(sw[0], sw[1]);
        assert_ne!(sw[0], sw[2]);
        assert_eq!(sw[2][0], [255, 255, 255, 255]);
        assert!(sw[1][0][0] < 60, "night land is dark");
    }

    /// Every built-in style draws the same classes and labels as the default one, so
    /// switching changes colours and nothing is lost.
    #[test]
    fn built_in_styles_cover_what_the_default_does() {
        use omp_core::schema::Schema;
        use omp_core::whitelist::{DEFAULT, Whitelist};
        let schema = Schema::from_whitelist(&Whitelist::parse(DEFAULT).unwrap());
        let v = list(Path::new("/nonexistent"), None);
        let compile = |e: &Entry| {
            let (style, warnings) = style::Style::compile(e.file.as_ref().unwrap(), &schema);
            assert!(warnings.is_empty(), "{}: {warnings:?}", e.id);
            style
        };
        let base = compile(&v[0]);
        for e in &v[1..] {
            let s = compile(e);
            assert_eq!(s.paints.len(), base.paints.len(), "{}", e.id);
            for (l, classes) in base.classes.iter().enumerate() {
                for (c, cs) in classes.iter().enumerate() {
                    assert_eq!(cs.is_some(), s.classes[l][c].is_some(), "{} {l}/{c}", e.id);
                    let sym = |st: &style::Style| {
                        st.symbols[l][c].as_ref().map(|y| {
                            (
                                y.label.as_ref().map(|l| l.priority),
                                y.icon.is_some(),
                                y.shields.is_some(),
                            )
                        })
                    };
                    assert_eq!(sym(&base), sym(&s), "{} {l}/{c}", e.id);
                }
            }
        }
        // The overlays differ in colour from the default's, not in width.
        for e in &v[1..] {
            let o = compile(e).overlay;
            assert_ne!(o, base.overlay);
            let w = |o: &style::Overlay| {
                (
                    o.route_width,
                    o.picked_width,
                    o.alternative_width,
                    o.halo_width,
                )
            };
            assert_eq!(w(&o), w(&base.overlay), "{}", e.id);
        }
    }

    #[test]
    fn custom_styles_are_listed_and_bad_ones_keep_their_error() {
        let dir = std::env::temp_dir().join(format!("omp-styles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(DIR)).unwrap();
        std::fs::write(dir.join(style::FILE), style::DEFAULT).unwrap();
        std::fs::write(
            dir.join(DIR).join("mine.toml"),
            "version = 1\nland = \"#fff\"",
        )
        .unwrap();
        std::fs::write(dir.join(DIR).join("notes.txt"), "x").unwrap();
        let v = list(&dir, None);
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids[3..], ["custom", "custom:mine.toml"]);
        assert_eq!(v[3].name, "Custom (style.toml)");
        assert!(v[4].file.as_ref().unwrap_err().contains("bad colour"));
        assert_eq!(v[4].name, "mine");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
