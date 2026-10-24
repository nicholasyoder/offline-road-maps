//! Loaded tiles, least recently used first out, within a byte budget.

use std::collections::HashMap;

use crate::tiles::TileId;

struct Entry<V> {
    value: V,
    bytes: usize,
    /// Frame in which it was last used.
    used: u64,
}

pub struct Cache<V> {
    map: HashMap<TileId, Entry<V>>,
    bytes: usize,
    pub budget: usize,
    frame: u64,
    /// Highest `bytes` seen, for the bench.
    pub peak: usize,
}

impl<V> Cache<V> {
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            bytes: 0,
            budget,
            frame: 0,
            peak: 0,
        }
    }

    /// Starts a new frame: tiles used from now on are protected from eviction until the next.
    pub fn next_frame(&mut self) {
        self.frame += 1;
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn contains(&self, id: TileId) -> bool {
        self.map.contains_key(&id)
    }

    /// The tile, marked as used in this frame.
    pub fn get(&mut self, id: TileId) -> Option<&V> {
        let frame = self.frame;
        self.map.get_mut(&id).map(|e| {
            e.used = frame;
            &e.value
        })
    }

    pub fn insert(&mut self, id: TileId, value: V, bytes: usize) {
        let e = Entry {
            value,
            bytes,
            used: self.frame,
        };
        if let Some(old) = self.map.insert(id, e) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.peak = self.peak.max(self.bytes);
        self.evict();
    }

    /// Drops the least recently used tiles until the cache fits its budget. Tiles used in the
    /// current frame stay, so a view that needs more than the budget still draws.
    fn evict(&mut self) {
        if self.bytes <= self.budget {
            return;
        }
        let mut old: Vec<_> = self
            .map
            .iter()
            .filter(|(_, e)| e.used < self.frame)
            .map(|(id, e)| (e.used, *id))
            .collect();
        old.sort_unstable();
        for (_, id) in old {
            if self.bytes <= self.budget {
                break;
            }
            if let Some(e) = self.map.remove(&id) {
                self.bytes -= e.bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recent_but_not_this_frame() {
        let t = |x| TileId::new(10, x, 0);
        let mut c = Cache::new(300);
        for x in 0..3 {
            c.next_frame();
            c.insert(t(x), x, 100);
        }
        c.next_frame();
        assert!(c.get(t(0)).is_some());
        c.insert(t(3), 3, 100);
        // t(1) was the least recently used.
        assert!(!c.contains(t(1)) && c.contains(t(0)) && c.contains(t(3)));
        assert_eq!(c.bytes(), 300);
        // Everything used this frame stays, even over budget.
        for x in [0, 2, 3] {
            c.get(t(x));
        }
        c.insert(t(4), 4, 100);
        assert_eq!((c.len(), c.bytes(), c.peak), (4, 400, 400));
        c.next_frame();
        c.insert(t(5), 5, 0);
        assert_eq!(c.bytes(), 300);
    }
}
