//! Finding packs: in `maps/` next to the executable, or at `--pack <dir>`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omp_core::pack::Pack;
use omp_core::schema::{self, Schema};
use omp_core::tiles::TileReader;

use crate::camera::lonlat_to_world;

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
}

pub fn open(first_chunk: &Path) -> Result<Opened, String> {
    let err = |e: std::io::Error| format!("{}: {e}", first_chunk.display());
    let pack = Pack::open(first_chunk).map_err(err)?;
    let [w, s, e, n] = pack.info.bbox.map(|v| f64::from(v) * 1e-7);
    let [x0, y0] = lonlat_to_world(w, n);
    let [x1, y1] = lonlat_to_world(e, s);
    let section = pack
        .section(schema::ID)
        .copied()
        .ok_or_else(|| format!("{}: the pack has no SCHM section", first_chunk.display()))?;
    let schema = Schema::decode(&pack.read_section(&section).map_err(err)?).map_err(err)?;
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
    })
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
}
