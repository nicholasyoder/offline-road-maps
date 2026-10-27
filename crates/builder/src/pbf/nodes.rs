//! Node location stores for the two strategies compared in step 0.3.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use memmap2::MmapMut;

use crate::geom::P;

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

/// A bitset over node IDs that only allocates the pages it touches. Inserts are atomic, so
/// parallel readers can mark nodes without collecting them first.
pub struct NodeSet {
    pages: Vec<OnceLock<Box<[AtomicU64]>>>,
}

const PAGE_BITS: u64 = 1 << 22;
const PAGE_WORDS: usize = (PAGE_BITS / 64) as usize;

impl Clone for NodeSet {
    fn clone(&self) -> Self {
        let copy = |words: &[AtomicU64]| -> Box<[AtomicU64]> {
            words
                .iter()
                .map(|w| AtomicU64::new(w.load(Ordering::Relaxed)))
                .collect()
        };
        Self {
            pages: self
                .pages
                .iter()
                .map(|p| p.get().map(|w| OnceLock::from(copy(w))).unwrap_or_default())
                .collect(),
        }
    }
}

impl NodeSet {
    pub fn new() -> Self {
        Self {
            pages: (0..DENSE_CAPACITY / PAGE_BITS)
                .map(|_| OnceLock::new())
                .collect(),
        }
    }

    /// The page's words and the word index and bit mask of `id`.
    fn locate(id: i64) -> Option<(usize, usize, u64)> {
        let id = u64::try_from(id).ok().filter(|&id| id < DENSE_CAPACITY)?;
        let bit = id % PAGE_BITS;
        Some((
            (id / PAGE_BITS) as usize,
            (bit / 64) as usize,
            1 << (bit % 64),
        ))
    }

    /// Adds `id`; returns whether it was already in the set.
    pub fn insert(&self, id: i64) -> bool {
        let Some((page, word, mask)) = Self::locate(id) else {
            return false;
        };
        let words =
            self.pages[page].get_or_init(|| (0..PAGE_WORDS).map(|_| AtomicU64::new(0)).collect());
        words[word].fetch_or(mask, Ordering::Relaxed) & mask != 0
    }

    pub fn contains(&self, id: i64) -> bool {
        Self::locate(id).is_some_and(|(page, word, mask)| {
            self.pages[page]
                .get()
                .is_some_and(|w| w[word].load(Ordering::Relaxed) & mask != 0)
        })
    }

    pub fn len(&self) -> u64 {
        self.pages
            .iter()
            .filter_map(OnceLock::get)
            .flat_map(|w| w.iter())
            .map(|w| u64::from(w.load(Ordering::Relaxed).count_ones()))
            .sum()
    }

    pub fn heap_bytes(&self) -> u64 {
        self.pages.iter().filter(|p| p.get().is_some()).count() as u64 * PAGE_BITS / 8
    }
}

/// Words per rank block. A block's rank is stored; the words before `id` within its block
/// are counted at lookup.
const RANK_WORDS: usize = 8;

/// Strategy 2: the locations of the nodes in a [`NodeSet`], one 8-byte slot per member,
/// addressed by the member's rank in the set. Slot value 0 means "not found in the file",
/// as in [`DenseIndex`]. Slots are atomic so the node pass can fill them in parallel.
pub struct RankedLocations {
    set: NodeSet,
    /// Per page: the number of members before each block of [`RANK_WORDS`] words.
    ranks: Vec<Box<[u64]>>,
    slots: Vec<AtomicU64>,
}

impl RankedLocations {
    pub fn new(set: NodeSet) -> Self {
        let mut total = 0;
        let ranks = set
            .pages
            .iter()
            .map(|p| match p.get() {
                None => Box::default(),
                Some(words) => words
                    .chunks(RANK_WORDS)
                    .map(|block| {
                        let r = total;
                        total += block
                            .iter()
                            .map(|w| u64::from(w.load(Ordering::Relaxed).count_ones()))
                            .sum::<u64>();
                        r
                    })
                    .collect(),
            })
            .collect();
        let slots = (0..total).map(|_| AtomicU64::new(0)).collect();
        Self { set, ranks, slots }
    }

    fn slot(&self, id: i64) -> Option<&AtomicU64> {
        let (page, word, mask) = NodeSet::locate(id)?;
        let words = self.set.pages[page].get()?;
        let w = words[word].load(Ordering::Relaxed);
        if w & mask == 0 {
            return None;
        }
        let block = word / RANK_WORDS;
        let before: u32 = words[block * RANK_WORDS..word]
            .iter()
            .map(|x| x.load(Ordering::Relaxed).count_ones())
            .sum();
        let rank = self.ranks[page][block] + u64::from(before + (w & (mask - 1)).count_ones());
        self.slots.get(rank as usize)
    }

    /// Stores `p` if `id` is in the set.
    pub fn set(&self, id: i64, p: P) {
        if let Some(s) = self.slot(id) {
            s.store(pack(p), Ordering::Relaxed);
        }
    }

    pub fn get(&self, id: i64) -> Option<P> {
        let v = self.slot(id)?.load(Ordering::Relaxed);
        (v != 0).then(|| unpack(v))
    }

    /// Members of the set, found in the file or not.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn heap_bytes(&self) -> u64 {
        self.set.heap_bytes()
            + self.ranks.iter().map(|r| r.len() as u64 * 8).sum::<u64>()
            + self.slots.len() as u64 * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_set() {
        let s = NodeSet::new();
        for id in [0, 63, 64, 13_000_000_000] {
            assert!(!s.insert(id));
        }
        assert!(s.insert(64));
        assert!(s.contains(63) && s.contains(13_000_000_000) && !s.contains(65));
        assert!(!s.contains(-1) && !s.contains(1 << 40));
        assert_eq!(s.len(), 4);
        assert_eq!(s.heap_bytes(), 2 * PAGE_BITS / 8);
    }

    #[test]
    fn ranked_locations() {
        let s = NodeSet::new();
        // Across words, blocks and pages.
        let ids = [5, 64, 600, 1_000, PAGE_BITS as i64 + 3, 13_000_000_000];
        for &id in &ids {
            s.insert(id);
        }
        let locs = RankedLocations::new(s);
        assert_eq!(locs.len(), ids.len());
        for (i, &id) in ids.iter().enumerate() {
            locs.set(id, [i as u32 + 1, 7]);
        }
        locs.set(6, [9, 9]);
        for (i, &id) in ids.iter().enumerate() {
            assert_eq!(locs.get(id), Some([i as u32 + 1, 7]));
        }
        assert_eq!(locs.get(6), None);
        assert_eq!(locs.get(-5), None);
    }
}
