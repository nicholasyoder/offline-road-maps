//! `META` for `build`: the sources and snapshot (from the `fetch` manifest), the attribution
//! and the build settings.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use omp_core::meta::{Meta, Source};
use omp_core::pack;
use omp_core::whitelist::Whitelist;
use sha2::{Digest, Sha256};

use crate::addr::{merge, store};
use crate::fetch::manifest::{FileEntry, Manifest};
use crate::{admin, search, street, tiles};

/// Where the pack's data came from: the manifest, and the sources and snapshot found in it.
pub struct Provenance {
    pub manifest: Manifest,
    /// The directory holding the manifest: the inputs directory.
    pub dir: PathBuf,
    pub sources: Vec<Source>,
    pub snapshot: String,
    /// Input files whose hash is not yet being checked against the manifest.
    unchecked: Vec<Input>,
    /// Threads hashing inputs, which [`Self::finish_hashing`] joins.
    hashing: Vec<JoinHandle<Result<()>>>,
}

/// An input file as the manifest describes it.
struct Input {
    path: PathBuf,
    sha256: String,
}

impl Provenance {
    /// The sources of the OSM `input` and of the water polygons, from the manifest at
    /// `manifest_path`.
    pub fn load(manifest_path: &Path, input: &Path, water: Option<&Path>) -> Result<Self> {
        let manifest = Manifest::load_or_default(manifest_path)?;
        let dir = manifest_path.parent().unwrap_or(Path::new(".")).to_owned();
        let osm_entry = manifest_entry(&manifest, &dir, input).cloned();
        let osm_entry = osm_entry.as_ref();
        let mut sources = match osm_entry {
            Some(e) => vec![source(e)],
            None => {
                eprintln!(
                    "  warning: {} is not in {}; META lists it without a hash",
                    input.display(),
                    manifest_path.display()
                );
                vec![Source {
                    name: "osm".into(),
                    url: file_label(input),
                    license: "ODbL-1.0".into(),
                    attribution: Some("© OpenStreetMap contributors".into()),
                    ..Source::default()
                }]
            }
        };
        let mut snapshot = snapshot(input, osm_entry);
        match water {
            Some(z) => {
                if let Some(e) = manifest_entry(&manifest, &dir, z) {
                    sources.push(source(e));
                    snapshot.push_str(&format!(
                        "; water {}",
                        e.downloaded.get(..10).unwrap_or(&e.downloaded)
                    ));
                }
            }
            None => eprintln!("  water: no water polygons, oceans left out"),
        }
        let mut p = Self {
            manifest,
            dir,
            sources,
            snapshot,
            unchecked: Vec::new(),
            hashing: Vec::new(),
        };
        if let Some(e) = osm_entry {
            p.expect(e, input)?;
        }
        if let Some(z) = water
            && let Some(e) = manifest_entry(&p.manifest, &p.dir, z)
        {
            p.expect(&e.clone(), z)?;
        }
        Ok(p)
    }

    /// Stops if `file`'s size is not the manifest's `e.size` (the file was changed or cut short
    /// since `fetch`), else queues it for [`Self::start_hashing`].
    fn expect(&mut self, e: &FileEntry, file: &Path) -> Result<()> {
        let size = std::fs::metadata(file)
            .with_context(|| format!("read {}", file.display()))?
            .len();
        if size != e.size {
            bail!(
                "{} is {size} bytes but the manifest says {}: run `fetch` again",
                file.display(),
                e.size
            );
        }
        self.unchecked.push(Input {
            path: file.to_owned(),
            sha256: e.sha256.clone(),
        });
        Ok(())
    }

    /// Hashes the inputs queued so far on a thread of their own, beside the build's work, to
    /// compare with the manifest. [`Self::finish_hashing`] reports a mismatch.
    pub fn start_hashing(&mut self) {
        let inputs = std::mem::take(&mut self.unchecked);
        if inputs.is_empty() {
            return;
        }
        self.hashing.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 20];
            for i in inputs {
                let mut f = std::fs::File::open(&i.path)
                    .with_context(|| format!("read {}", i.path.display()))?;
                let mut h = Sha256::new();
                loop {
                    let n = f
                        .read(&mut buf)
                        .with_context(|| format!("read {}", i.path.display()))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                if !hex::encode(h.finalize()).eq_ignore_ascii_case(&i.sha256) {
                    bail!(
                        "{} does not match the manifest's SHA-256: run `fetch` again",
                        i.path.display()
                    );
                }
            }
            Ok(())
        }));
    }

    /// Waits for the hashing of every input and stops at the first that does not match.
    pub fn finish_hashing(&mut self) -> Result<()> {
        self.start_hashing();
        for h in std::mem::take(&mut self.hashing) {
            h.join()
                .map_err(|_| anyhow::anyhow!("an input's hash check panicked"))??;
        }
        Ok(())
    }

    /// Adds the source of another input file, `what` in the snapshot ("tz 2026e"), if the
    /// manifest has it.
    pub fn add_input(&mut self, file: &Path, what: &str) -> Result<()> {
        match manifest_entry(&self.manifest, &self.dir, file).cloned() {
            Some(e) => {
                self.sources.push(source(&e));
                let date = e.downloaded.get(..10).unwrap_or(&e.downloaded);
                let id = e.source_id.as_deref().unwrap_or(date);
                self.snapshot.push_str(&format!("; {what} {id}"));
                self.expect(&e, file)?;
            }
            None => eprintln!(
                "  warning: {} is not in the manifest; META leaves it out",
                file.display()
            ),
        }
        Ok(())
    }

    /// Adds the address files read, by manifest path.
    pub fn add_files(&mut self, files: &[String]) -> Result<()> {
        for f in files {
            match self.manifest.files.iter().find(|e| e.path == *f).cloned() {
                Some(e) => {
                    self.sources.push(source(&e));
                    self.expect(&e, &self.dir.join(f))?;
                }
                None => eprintln!("  warning: {f} is not in the manifest"),
            }
        }
        Ok(())
    }
}

/// The manifest entry for `file`, matched by canonical path.
fn manifest_entry<'m>(manifest: &'m Manifest, dir: &Path, file: &Path) -> Option<&'m FileEntry> {
    let want = file.canonicalize().ok()?;
    manifest
        .files
        .iter()
        .find(|e| dir.join(&e.path).canonicalize().ok().as_ref() == Some(&want))
}

fn source(e: &FileEntry) -> Source {
    Source {
        name: match &e.source_id {
            Some(id) => format!("{} {id}", e.source),
            None => e.source.clone(),
        },
        url: e.url.clone(),
        date: e.downloaded.clone(),
        sha256: e.sha256.clone(),
        license: e.license.name.clone(),
        license_url: e.license.url.clone(),
        attribution: e.license.attribution.clone(),
    }
}

/// `osm 2026-10-01` from a Geofabrik file name's `-YYMMDD`, else the download date.
pub fn snapshot(input: &Path, entry: Option<&FileEntry>) -> String {
    let file = input.file_name().unwrap_or_default().to_string_lossy();
    let stem = file.split('.').next().unwrap_or_default();
    if let Some((_, d)) = stem.rsplit_once('-')
        && d.len() == 6
        && d.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("osm 20{}-{}-{}", &d[0..2], &d[2..4], &d[4..6]);
    }
    match entry {
        Some(e) => format!("osm {}", e.downloaded.get(..10).unwrap_or(&e.downloaded)),
        None => "osm".into(),
    }
}

/// One line per distinct attribution, with its license.
pub fn attribution(sources: &[Source]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for s in sources {
        let line = match &s.attribution {
            Some(a) => format!("{a}, {}", s.license),
            None => s.license.clone(),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// What `META`'s settings record about the build.
pub struct Settings<'a> {
    pub wl: &'a Whitelist,
    pub zstd_level: i32,
    pub postcodes: usize,
    /// The address sections' sizes and the subdivisions merged, if any.
    pub addresses: Option<(&'a store::Sizes, &'a [merge::SubRow])>,
    pub exclude_license: &'a [String],
    pub input: &'a Path,
    pub water: Option<&'a Path>,
    pub clip_poly: Option<&'a Path>,
    pub tz: Option<&'a Path>,
}

/// `META` for a pack of `region` covering `bbox` (west, south, east, north).
pub fn meta(
    region: &str,
    bbox: [f64; 4],
    build_time: i64,
    sources: Vec<Source>,
    s: &Settings,
) -> Meta {
    let mut settings = serde_json::Map::new();
    settings.insert("whitelist_version".into(), s.wl.version.into());
    settings.insert("encoding".into(), "V4".into());
    settings.insert("zstd_level".into(), s.zstd_level.into());
    settings.insert("max_zoom".into(), tiles::MAX_ZOOM.into());
    settings.insert("admin_tolerance_m".into(), admin::PACK_TOLERANCE_M.into());
    settings.insert("street_tolerance_m".into(), street::SIMPLIFY_M.into());
    settings.insert("street_max_m".into(), street::STREET_MAX_M.into());
    settings.insert("grid_zoom".into(), omp_core::grid::ZOOM.into());
    settings.insert("search_stop_min".into(), search::STOP_MIN.into());
    settings.insert(
        "search_block_kib".into(),
        (search::encode::BLOCK >> 10).into(),
    );
    settings.insert("postcodes".into(), s.postcodes.into());
    if let Some((sizes, rows)) = s.addresses {
        settings.insert("addresses".into(), (sizes.addresses + sizes.points).into());
        settings.insert("address_block_kib".into(), (store::BLOCK >> 10).into());
        settings.insert("attach_m".into(), merge::ATTACH_M.into());
        settings.insert("dedup_m".into(), merge::DEDUP_M.into());
        let isos: Vec<String> = rows.iter().map(|r| r.iso.clone()).collect();
        settings.insert("address_subdivisions".into(), isos.join(",").into());
        if !s.exclude_license.is_empty() {
            settings.insert("exclude_license".into(), s.exclude_license.join(",").into());
        }
    }
    settings.insert("input".into(), file_label(s.input).into());
    if let Some(z) = s.water {
        settings.insert("water".into(), file_label(z).into());
    }
    if let Some(z) = s.tz {
        settings.insert("tz".into(), file_label(z).into());
        settings.insert("tz_tolerance_m".into(), crate::tz::TOLERANCE_M.into());
    }
    if let Some(p) = s.clip_poly {
        settings.insert("clip_poly".into(), file_label(p).into());
    }
    Meta {
        region: region.to_owned(),
        bbox,
        build_time,
        builder_version: env!("CARGO_PKG_VERSION").into(),
        format_version: format!("{}.{}", pack::MAJOR, pack::MINOR),
        attribution: attribution(&sources),
        sources,
        settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_from_file_name() {
        assert_eq!(
            snapshot(Path::new("ohio-261001.osm.pbf"), None),
            "osm 2026-10-01"
        );
    }

    #[test]
    fn attribution_lists_each_once() {
        let s = |a: Option<&str>, l: &str| Source {
            attribution: a.map(Into::into),
            license: l.into(),
            ..Source::default()
        };
        let a = attribution(&[
            s(Some("© OpenStreetMap contributors"), "ODbL-1.0"),
            s(Some("© OpenStreetMap contributors"), "ODbL-1.0"),
            s(None, "CC0"),
        ]);
        assert_eq!(a, "© OpenStreetMap contributors, ODbL-1.0\nCC0");
    }

    /// A provenance whose manifest lists `name` with `size` and `sha256`.
    fn provenance_of(dir: &Path, name: &str, size: u64, sha256: &str) -> Provenance {
        let entry = FileEntry {
            path: name.into(),
            source: "osm".into(),
            source_id: None,
            url: String::new(),
            metadata_url: None,
            downloaded: "2026-10-01T00:00:00Z".into(),
            size,
            sha256: sha256.into(),
            md5: None,
            license: crate::fetch::manifest::License {
                name: "ODbL-1.0".into(),
                url: None,
                attribution_required: None,
                attribution: None,
                share_alike: None,
            },
        };
        Provenance {
            manifest: Manifest { files: vec![entry] },
            dir: dir.to_owned(),
            sources: Vec::new(),
            snapshot: String::new(),
            unchecked: Vec::new(),
            hashing: Vec::new(),
        }
    }

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn inputs_are_checked_against_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"abc").unwrap();
        let file = dir.path().join("a.bin");

        let mut p = provenance_of(dir.path(), "a.bin", 3, ABC);
        p.add_input(&file, "x").unwrap();
        p.finish_hashing().unwrap();

        let mut p = provenance_of(dir.path(), "a.bin", 4, ABC);
        let e = p.add_input(&file, "x").unwrap_err().to_string();
        assert!(e.contains("a.bin") && e.contains("fetch"), "{e}");

        let mut p = provenance_of(dir.path(), "a.bin", 3, &"0".repeat(64));
        p.add_input(&file, "x").unwrap();
        let e = p.finish_hashing().unwrap_err().to_string();
        assert!(e.contains("a.bin") && e.contains("SHA-256"), "{e}");
    }
}
