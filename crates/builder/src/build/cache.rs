//! The stage cache of `build --reuse-stages`, for dev loops. A stage's output is saved in
//! `<temp>/cache/` and loaded again by a later build that has the same key for it. The key
//! covers the input file (size, modification time, and a hash of its first and last MiB),
//! the builder's version, the stage's version below and the options the stage depends on.
//! It does not cover the code: after changing what a stage outputs, bump its version here
//! (or build without `--reuse-stages`).

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use xxhash_rust::xxh3::xxh3_128;

/// The admin areas as read ([`crate::admin::read::AdminData`]).
pub const ADMIN: u32 = 1;
/// The band reads and tile cut: the tile store, its entries, and what the bands collect.
pub const TILES: u32 = 2;
/// The OSM addresses and street ways, and their merge.
pub const ADDRESSES: u32 = 1;
/// The car routing graph, with the profile it was built with.
pub const GRAPH: u32 = 2;
/// The contraction hierarchy of the graph; its key also holds the graph's version.
pub const CH: u32 = 1;

pub struct Cache {
    dir: PathBuf,
    /// The input's part of every key.
    input: String,
}

/// A stage's key: its file name stem, `<stage>-<hash>`.
pub struct Key {
    stage: &'static str,
    stem: String,
}

impl Cache {
    pub fn new(temp: &Path, input: &Path) -> Result<Self> {
        let dir = temp.join("cache");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let mut f = File::open(input).with_context(|| format!("open {}", input.display()))?;
        let len = f.metadata()?.len();
        let mut ends = Vec::new();
        f.by_ref().take(1 << 20).read_to_end(&mut ends)?;
        f.seek(SeekFrom::Start(len.saturating_sub(1 << 20)))?;
        f.read_to_end(&mut ends)?;
        Ok(Cache {
            dir,
            input: format!(
                "{} {} {:032x}",
                fingerprint(input)?,
                env!("CARGO_PKG_VERSION"),
                xxh3_128(&ends)
            ),
        })
    }

    /// The key of `stage` with `parts`: its versions and options, in a form that changes
    /// when they do.
    pub fn key(&self, stage: &'static str, parts: &str) -> Key {
        let hash = xxh3_128(format!("{stage}\n{}\n{parts}", self.input).as_bytes());
        Key {
            stage,
            stem: format!("{stage}-{:016x}", hash as u64),
        }
    }

    /// A file of the stage beside its saved output, such as the tile store.
    pub fn file(&self, key: &Key, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", key.stem))
    }

    fn load<T: DeserializeOwned>(&self, key: &Key) -> Result<Option<T>> {
        let path = self.file(key, "bin");
        // Written whole, then renamed into place.
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(None);
        };
        match postcard::from_bytes(&bytes) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                eprintln!(
                    "  cache: {} is unreadable ({e}); building it again",
                    path.display()
                );
                Ok(None)
            }
        }
    }

    /// Saves `v` as the stage's output, and deletes the stage's files of other keys.
    fn save<T: Serialize>(&self, key: &Key, v: &T) -> Result<()> {
        for e in std::fs::read_dir(&self.dir)? {
            let e = e?;
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("{}-", key.stage)) && !name.starts_with(&key.stem) {
                std::fs::remove_file(e.path())?;
            }
        }
        let path = self.file(key, "bin");
        let tmp = self.file(key, "bin.tmp");
        let mut w = BufWriter::new(File::create(&tmp)?);
        postcard::to_io(v, &mut w).with_context(|| format!("write {}", tmp.display()))?;
        w.into_inner()?.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Deletes the stage's saved output, so the next [`stage`] makes it again.
    pub fn forget(&self, key: &Key) {
        let _ = std::fs::remove_file(self.file(key, "bin"));
    }
}

/// `make()`'s output, or with a cache, the output saved for `key` if there is one; a new
/// output is saved.
pub fn stage<T: Serialize + DeserializeOwned>(
    at: Option<(&Cache, &Key)>,
    make: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let Some((cache, key)) = at else {
        return make();
    };
    let t = Instant::now();
    if let Some(v) = cache.load(key)? {
        eprintln!(
            "  cache: {} reused, {:.1} s",
            key.stem,
            t.elapsed().as_secs_f64()
        );
        return Ok(v);
    }
    let v = make()?;
    let t = Instant::now();
    cache.save(key, &v)?;
    eprintln!(
        "  cache: {} saved, {:.1} s",
        key.stem,
        t.elapsed().as_secs_f64()
    );
    Ok(v)
}

/// A file's path, size and modification time; `-` if there is no file.
pub fn fingerprint(path: &Path) -> Result<String> {
    let Ok(m) = std::fs::metadata(path) else {
        return Ok(format!("{} -", path.display()));
    };
    let mtime = m.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{} {} {mtime}", path.display(), m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_are_saved_reused_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.osm.pbf");
        std::fs::write(&input, b"one").unwrap();
        let cache = Cache::new(dir.path(), &input).unwrap();
        let one = cache.key("admin", "1");
        let made: Vec<u32> = stage(Some((&cache, &one)), || Ok(vec![1, 2, 3])).unwrap();
        let reused: Vec<u32> = stage(Some((&cache, &one)), || panic!("made again")).unwrap();
        assert_eq!(made, reused);
        // Another key of the stage replaces it; other stages stay.
        let tiles = cache.key("tiles", "1");
        stage(Some((&cache, &tiles)), || Ok(7u8)).unwrap();
        let two = cache.key("admin", "2");
        assert_eq!(stage(Some((&cache, &two)), || Ok(vec![4u32])).unwrap(), [4]);
        assert!(!cache.file(&one, "bin").exists());
        assert!(cache.file(&tiles, "bin").exists());
        // Without a cache, the stage is always made.
        assert_eq!(stage(None, || Ok(5u8)).unwrap(), 5);
        // A changed input changes every key.
        std::fs::write(&input, b"other").unwrap();
        let changed = Cache::new(dir.path(), &input).unwrap();
        assert_ne!(changed.key("admin", "2").stem, two.stem);
    }
}
