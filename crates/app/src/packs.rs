//! Finding packs: in `maps/` next to the executable, or at `--pack <dir>`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omp_core::meta::{self, Meta};
use omp_core::pack::Pack;
use omp_core::schema::{self, Schema};
use omp_core::tiles::TileReader;

use crate::camera::lonlat_to_world;
use crate::scale::Units;

/// First-chunk files (`<name>.omp.000`) in `dir` and its direct subfolders, sorted. `dir` may
/// also be a first-chunk file itself.
pub fn find(dir: &Path) -> Vec<PathBuf> {
    if is_first_chunk(dir) {
        return vec![dir.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut scan = |d: &Path, deeper: &mut Vec<PathBuf>| {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if is_first_chunk(&p) {
                out.push(p);
            } else if p.is_dir() {
                deeper.push(p);
            }
        }
    };
    let mut subdirs = Vec::new();
    scan(dir, &mut subdirs);
    for d in subdirs {
        scan(&d, &mut Vec::new());
    }
    out.sort();
    out
}

fn is_first_chunk(p: &Path) -> bool {
    p.is_file()
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".omp.000"))
}

/// The folder a pack is known by: the one holding its chunks.
pub fn pack_dir(first_chunk: &Path) -> PathBuf {
    first_chunk
        .parent()
        .map_or_else(|| first_chunk.to_path_buf(), Path::to_path_buf)
}

/// An open pack.
pub struct Opened {
    pub first_chunk: PathBuf,
    pub reader: Arc<TileReader>,
    /// The header's bounding box in world coordinates, `[x0, y0, x1, y1]`.
    pub bounds: [f64; 4],
    /// Layer, class and prop names, which the style is resolved against.
    pub schema: Schema,
    /// For "About this map"; a pack opens without it.
    pub meta: Result<Meta, String>,
}

pub fn open(first_chunk: &Path) -> Result<Opened, String> {
    let err = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => format!(
            "{}: the file ends too early; it may not have been copied completely",
            first_chunk.display()
        ),
        _ => format!("{}: {e}", first_chunk.display()),
    };
    let pack = Pack::open(first_chunk).map_err(err)?;
    let [w, s, e, n] = pack.info.bbox.map(|v| f64::from(v) * 1e-7);
    let [x0, y0] = lonlat_to_world(w, n);
    let [x1, y1] = lonlat_to_world(e, s);
    let section = pack
        .section(schema::ID)
        .copied()
        .ok_or_else(|| format!("{}: the pack has no SCHM section", first_chunk.display()))?;
    let schema = Schema::decode(&pack.read_section(&section).map_err(err)?).map_err(err)?;
    let meta = match pack.section(meta::ID) {
        Some(s) => pack
            .read_section(s)
            .and_then(|b| Meta::decode(&b))
            .map_err(|e| format!("META could not be read: {e}")),
        None => Err("the pack has no META section".into()),
    };
    let reader = TileReader::open(pack).map_err(err)?;
    let bounds = if x1 > x0 && y1 > y0 {
        [x0, y0, x1, y1]
    } else {
        [0.0, 0.0, 1.0, 1.0]
    };
    Ok(Opened {
        first_chunk: first_chunk.to_path_buf(),
        reader: Arc::new(reader),
        bounds,
        schema,
        meta,
    })
}

/// The scale bar's units for a pack: miles for a US pack, kilometres for a Canadian or
/// Mexican one, both for a pack that covers several countries or is not known. Told by the
/// OSM extract's URL (Geofabrik's `north-america/us/…`, `…/canada/…`, `…/mexico…`).
pub fn default_units(meta: Option<&Meta>) -> Units {
    let mut found = None;
    for s in meta.into_iter().flat_map(|m| &m.sources) {
        if !s.name.starts_with("osm") {
            continue;
        }
        let url = s.url.to_ascii_lowercase();
        let u = if url.contains("/us/") || url.contains("/us-") {
            Units::Miles
        } else if url.contains("/canada") || url.contains("/mexico") {
            Units::Kilometres
        } else {
            Units::Both
        };
        if found.is_some_and(|f| f != u) {
            return Units::Both;
        }
        found = Some(u);
    }
    found.unwrap_or(Units::Both)
}

/// The About window's rows for a pack's sources: one per source, or one per kind (the
/// name's first word: `openaddresses`, `tiger`) when a kind has several files, with their
/// count per license, commonest first, and the range of download dates.
pub fn source_rows(sources: &[meta::Source]) -> Vec<(String, String)> {
    let kind = |s: &meta::Source| s.name.split(' ').next().unwrap_or_default().to_owned();
    let day = |s: &meta::Source| s.date.get(..10).unwrap_or(&s.date).to_owned();
    let mut kinds: Vec<String> = Vec::new();
    for s in sources {
        if !kinds.contains(&kind(s)) {
            kinds.push(kind(s));
        }
    }
    kinds
        .into_iter()
        .map(|k| {
            let of: Vec<&meta::Source> = sources.iter().filter(|s| kind(s) == k).collect();
            if let [one] = of[..] {
                return (
                    one.name.clone(),
                    format!("{}, downloaded {}", one.license, day(one)),
                );
            }
            let mut licenses: Vec<(&str, usize)> = Vec::new();
            for s in &of {
                match licenses.iter_mut().find(|l| l.0 == s.license) {
                    Some(l) => l.1 += 1,
                    None => licenses.push((&s.license, 1)),
                }
            }
            licenses.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let mut parts: Vec<String> = licenses
                .iter()
                .take(3)
                .map(|(l, n)| format!("{n} {l}"))
                .collect();
            if licenses.len() > 3 {
                let rest: usize = licenses[3..].iter().map(|l| l.1).sum();
                parts.push(format!(
                    "{rest} under {} other licenses",
                    licenses.len() - 3
                ));
            }
            let days: Vec<String> = of.iter().map(|s| day(s)).collect();
            let (first, last) = (days.iter().min().unwrap(), days.iter().max().unwrap());
            let when = if first == last {
                format!("downloaded {first}")
            } else {
                format!("downloaded {first} to {last}")
            };
            (
                k,
                format!("{} files ({}), {when}", of.len(), parts.join(", ")),
            )
        })
        .collect()
}

/// `YYYY-MM-DD` of Unix seconds, UTC.
pub fn date(unix: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_first_chunks_one_level_down() {
        let d = tempfile::tempdir().unwrap();
        let touch = |p: &str| {
            let p = d.path().join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        };
        for p in [
            "ohio/ohio.omp.000",
            "ohio/ohio.omp.001",
            "na.omp.000",
            "a/b/deep.omp.000",
            "x.txt",
        ] {
            touch(p);
        }
        let found = find(d.path());
        assert_eq!(
            found,
            vec![
                d.path().join("na.omp.000"),
                d.path().join("ohio/ohio.omp.000")
            ]
        );
        assert_eq!(find(&found[1]), vec![found[1].clone()]);
        assert_eq!(pack_dir(&found[1]), d.path().join("ohio"));
    }

    #[test]
    fn units_follow_the_country() {
        let meta = |urls: &[&str]| Meta {
            sources: urls
                .iter()
                .map(|u| omp_core::meta::Source {
                    name: "osm x".into(),
                    url: (*u).into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let gf = "https://download.geofabrik.de/";
        let us = format!("{gf}north-america/us/ohio-261001.osm.pbf");
        let ca = format!("{gf}north-america/canada/quebec-261001.osm.pbf");
        let mx = format!("{gf}north-america/mexico-261001.osm.pbf");
        let na = format!("{gf}north-america-261001.osm.pbf");
        assert_eq!(default_units(Some(&meta(&[&us]))), Units::Miles);
        assert_eq!(default_units(Some(&meta(&[&ca]))), Units::Kilometres);
        assert_eq!(default_units(Some(&meta(&[&mx]))), Units::Kilometres);
        assert_eq!(default_units(Some(&meta(&[&na]))), Units::Both);
        assert_eq!(default_units(Some(&meta(&[&us, &ca]))), Units::Both);
        assert_eq!(default_units(None), Units::Both);
    }

    #[test]
    fn sources_grouped_by_kind() {
        let src = |name: &str, license: &str, date: &str| omp_core::meta::Source {
            name: name.into(),
            license: license.into(),
            date: date.into(),
            ..Default::default()
        };
        let rows = source_rows(&[
            src("osm", "ODbL-1.0", "2026-10-02T10:00:00Z"),
            src(
                "openaddresses us/oh/a",
                "unspecified",
                "2026-10-03T00:00:00Z",
            ),
            src("openaddresses us/oh/b", "CC0", "2026-10-05T00:00:00Z"),
            src(
                "openaddresses us/oh/c",
                "unspecified",
                "2026-10-04T00:00:00Z",
            ),
        ]);
        assert_eq!(
            rows,
            [
                ("osm".into(), "ODbL-1.0, downloaded 2026-10-02".into()),
                (
                    "openaddresses".into(),
                    "3 files (2 unspecified, 1 CC0), downloaded 2026-10-03 to 2026-10-05".into()
                ),
            ]
        );
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        // inspect: "built 2026-10-25T04:06:55Z".
        assert_eq!(date(1_792_901_215), "2026-10-25");
        assert_eq!(date(951_782_400), "2000-02-29");
    }
}
