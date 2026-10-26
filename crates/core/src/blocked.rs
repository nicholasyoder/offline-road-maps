//! The layout every street, search, address, admin polygon and grid section shares
//! (`docs/format.md`): `count:u32 nblocks:u32 (first:u32 offset:u32 length:u32)* block*`,
//! each block one zstd frame, offsets from the section's start. Also a small cache of decoded
//! blocks, shared by the readers.

use std::collections::HashMap;
use std::hash::Hash;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use zstd::zstd_safe::DCtx;

use crate::pack::{Pack, Section};
use crate::varint::{Reader, invalid};
use crate::zframe;

/// A blocked section's index, held in RAM.
pub struct Blocked {
    pub section: Section,
    /// What the section counts: streets, entries, addresses, cells, …
    pub count: u32,
    /// Per block: its `first` key and its frame's offset and length.
    firsts: Vec<u32>,
    frames: Vec<(u32, u32)>,
}

impl Blocked {
    /// Reads the index of section `id`, or `None` if the pack has no such section.
    pub fn open(pack: &Pack, id: [u8; 4]) -> io::Result<Option<Self>> {
        let Some(&section) = pack.section(id) else {
            return Ok(None);
        };
        let name = String::from_utf8_lossy(&id).into_owned();
        if section.spanning() || section.len < 8 || section.len > u32::MAX.into() {
            return Err(invalid(format!("bad {name} section")));
        }
        let head = pack.read(section.offset, 8)?;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let (count, n) = (u32_at(&head, 0), u32_at(&head, 4) as usize);
        let index_len = 12 * n as u64;
        if 8 + index_len > section.len {
            return Err(invalid(format!("{name} block index past the end")));
        }
        let index = pack.read(section.offset + 8, index_len as usize)?;
        let mut firsts = Vec::with_capacity(n);
        let mut frames = Vec::with_capacity(n);
        let mut end = 8 + index_len;
        for i in 0..n {
            let (first, offset, len) = (
                u32_at(&index, 12 * i),
                u32_at(&index, 12 * i + 4),
                u32_at(&index, 12 * i + 8),
            );
            if u64::from(offset) < end || u64::from(offset) + u64::from(len) > section.len {
                return Err(invalid(format!("{name} block {i} is out of bounds")));
            }
            if firsts.last().is_some_and(|&f| first < f) {
                return Err(invalid(format!("{name} blocks out of order")));
            }
            end = u64::from(offset) + u64::from(len);
            firsts.push(first);
            frames.push((offset, len));
        }
        Ok(Some(Self {
            section,
            count,
            firsts,
            frames,
        }))
    }

    pub fn blocks(&self) -> usize {
        self.firsts.len()
    }

    pub fn first(&self, block: usize) -> u32 {
        self.firsts[block]
    }

    /// The last block whose `first` is at most `key`.
    pub fn find(&self, key: u32) -> Option<usize> {
        self.firsts.partition_point(|&f| f <= key).checked_sub(1)
    }

    /// Reads and decompresses block `block`; also returns the compressed bytes read.
    pub fn read(&self, pack: &Pack, block: usize) -> io::Result<(Vec<u8>, u64)> {
        let (offset, len) = self.frames[block];
        let frame = pack.read(self.section.offset + u64::from(offset), len as usize)?;
        let raw = zframe::decompress(&mut DCtx::create(), &frame, None)?;
        Ok((raw, u64::from(len)))
    }

    pub fn heap_bytes(&self) -> usize {
        self.firsts.capacity() * 4 + self.frames.capacity() * 8
    }
}

pub fn string(r: &mut Reader) -> io::Result<String> {
    let n = r.count()?;
    String::from_utf8(r.bytes(n)?.to_vec()).map_err(|_| invalid("bad UTF-8"))
}

pub fn strings(r: &mut Reader) -> io::Result<Vec<String>> {
    (0..r.count()?).map(|_| string(r)).collect()
}

/// Decoded blocks, least recently used first out once they hold more than `budget` bytes.
/// `Sync`, so the readers that own one can be shared.
pub struct Cache<K, V> {
    budget: usize,
    inner: Mutex<CacheInner<K, V>>,
    /// Compressed bytes read from the pack on misses.
    pub bytes_read: AtomicU64,
}

struct CacheInner<K, V> {
    map: HashMap<K, (Arc<V>, usize, u64)>,
    held: usize,
    tick: u64,
}

impl<K: Copy + Eq + Hash, V> Cache<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            inner: Mutex::new(CacheInner {
                map: HashMap::new(),
                held: 0,
                tick: 0,
            }),
            bytes_read: AtomicU64::new(0),
        }
    }

    /// The value for `key`, from `load` on a miss: `(value, bytes it holds, bytes read)`.
    pub fn get(
        &self,
        key: K,
        load: impl FnOnce() -> io::Result<(V, usize, u64)>,
    ) -> io::Result<Arc<V>> {
        {
            let mut c = self.inner.lock().unwrap();
            c.tick += 1;
            let tick = c.tick;
            if let Some((v, _, t)) = c.map.get_mut(&key) {
                *t = tick;
                return Ok(v.clone());
            }
        }
        let (v, bytes, read) = load()?;
        self.bytes_read.fetch_add(read, Ordering::Relaxed);
        let v = Arc::new(v);
        let mut c = self.inner.lock().unwrap();
        let tick = c.tick;
        if let Some((_, old, _)) = c.map.insert(key, (v.clone(), bytes, tick)) {
            c.held -= old;
        }
        c.held += bytes;
        // Keep the newest value even when it alone is over the budget.
        while c.held > self.budget && c.map.len() > 1 {
            let oldest = c.map.iter().min_by_key(|(_, e)| e.2).map(|(k, _)| *k);
            let Some(k) = oldest else { break };
            let (_, b, _) = c.map.remove(&k).unwrap();
            c.held -= b;
        }
        Ok(v)
    }

    /// Bytes the cached values hold, as their loaders counted them.
    pub fn held(&self) -> usize {
        self.inner.lock().unwrap().held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_evicts_the_least_recently_used() {
        let c: Cache<u32, u32> = Cache::new(30);
        let load = |v| move || Ok((v, 10, 1));
        for k in 0..3 {
            c.get(k, load(k)).unwrap();
        }
        c.get(0, load(99)).unwrap();
        c.get(3, load(3)).unwrap();
        // 1 was the least recently used; 0 was used again, so it stays.
        assert_eq!(*c.get(0, load(99)).unwrap(), 0);
        assert_eq!(*c.get(1, load(11)).unwrap(), 11);
        assert_eq!(c.held(), 30);
        assert_eq!(c.bytes_read.load(Ordering::Relaxed), 5);
    }
}
