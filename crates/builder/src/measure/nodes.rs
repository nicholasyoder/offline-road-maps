//! Node location stores for the two strategies compared in step 0.3.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use memmap2::MmapMut;

use super::geom::P;

/// Room for node IDs below 2^34 (about 17 billion; the highest in 2026 is around 13.5 billion).
const DENSE_CAPACITY: u64 = 1 << 34;

fn pack(p: P) -> u64 {
    (u64::from(p[0]) << 32) | u64::from(p[1])
}

fn unpack(v: u64) -> P {
    [(v >> 32) as u32, v as u32]
}

/// Strategy 1: one 8-byte slot per node ID in a file-backed mmap. The file is sparse, so it
/// must live on a filesystem with holes (ext4, not exFAT). Slot value 0 means "no node"; the
/// world corner it would decode to is never a real location.
pub struct DenseIndex {
    path: PathBuf,
    file: File,
    map: MmapMut,
}

impl DenseIndex {
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        file.set_len(DENSE_CAPACITY * 8)?;
        #[allow(unsafe_code)]
        // SAFETY: the file was just created and truncated by us and nothing else maps it.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            path: path.to_owned(),
            file,
            map,
        })
    }

    fn slots(&self) -> &[AtomicU64] {
        #[allow(unsafe_code)]
        // SAFETY: the map is page-aligned, a multiple of 8 bytes long and lives as long as
        // `self`. All access goes through these atomics.
        unsafe {
            std::slice::from_raw_parts(self.map.as_ptr().cast::<AtomicU64>(), self.map.len() / 8)
        }
    }

    pub fn set(&self, id: i64, p: P) -> Result<()> {
        ensure!(
            (0..DENSE_CAPACITY as i64).contains(&id),
            "node id {id} outside the dense index"
        );
        self.slots()[id as usize].store(pack(p), Ordering::Relaxed);
        Ok(())
    }

    pub fn get(&self, id: i64) -> Option<P> {
        let v = self
            .slots()
            .get(usize::try_from(id).ok()?)?
            .load(Ordering::Relaxed);
        (v != 0).then(|| unpack(v))
    }

    /// Bytes actually allocated on disk (the full length where holes cannot be measured).
    pub fn disk_bytes(&self) -> u64 {
        #[cfg(unix)]
        let bytes = std::os::unix::fs::MetadataExt::blocks;
        #[cfg(not(unix))]
        let bytes = |m: &std::fs::Metadata| m.len() / 512;
        self.file.metadata().map(|m| bytes(&m) * 512).unwrap_or(0)
    }
}

impl Drop for DenseIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A bitset over node IDs that only allocates the pages it touches.
pub struct NodeSet {
    pages: Vec<Option<Box<[u64]>>>,
}

const PAGE_BITS: u64 = 1 << 22;

impl NodeSet {
    pub fn new() -> Self {
        Self {
            pages: (0..DENSE_CAPACITY / PAGE_BITS).map(|_| None).collect(),
        }
    }

    pub fn insert(&mut self, id: i64) {
        let Ok(id) = u64::try_from(id) else { return };
        let Some(page) = self.pages.get_mut((id / PAGE_BITS) as usize) else {
            return;
        };
        let page = page.get_or_insert_with(|| vec![0; (PAGE_BITS / 64) as usize].into());
        let bit = id % PAGE_BITS;
        page[(bit / 64) as usize] |= 1 << (bit % 64);
    }

    pub fn contains(&self, id: i64) -> bool {
        let Ok(id) = u64::try_from(id) else {
            return false;
        };
        let Some(Some(page)) = self.pages.get((id / PAGE_BITS) as usize) else {
            return false;
        };
        let bit = id % PAGE_BITS;
        page[(bit / 64) as usize] & (1 << (bit % 64)) != 0
    }

    pub fn heap_bytes(&self) -> u64 {
        self.pages.iter().flatten().count() as u64 * PAGE_BITS / 8
    }
}

/// Strategy 2: the locations of needed nodes only, sorted by ID.
pub struct SparseLocations {
    ids: Vec<i64>,
    locs: Vec<P>,
}

impl SparseLocations {
    pub fn new(mut v: Vec<(i64, P)>) -> Self {
        v.sort_unstable_by_key(|e| e.0);
        let (ids, locs) = v.into_iter().unzip();
        Self { ids, locs }
    }

    pub fn get(&self, id: i64) -> Option<P> {
        self.ids.binary_search(&id).ok().map(|i| self.locs[i])
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_set() {
        let mut s = NodeSet::new();
        for id in [0, 63, 64, 13_000_000_000] {
            s.insert(id);
        }
        assert!(s.contains(63) && s.contains(13_000_000_000) && !s.contains(65));
        assert_eq!(s.heap_bytes(), 2 * PAGE_BITS / 8);
    }

    #[test]
    fn sparse_lookup() {
        let s = SparseLocations::new(vec![(7, [1, 2]), (3, [5, 6])]);
        assert_eq!(s.get(3), Some([5, 6]));
        assert_eq!(s.get(4), None);
    }
}
