//! The tile store's payloads, read back, and the zstd dictionaries trained on them.

use std::collections::BTreeMap;
use std::fs::File;

use rayon::prelude::*;

use super::Entry;
use omp_core::pack::read_at;
use omp_core::tile::{Dictionaries, zoom_band};

const DICT_SIZE: usize = 64 << 10;
/// Training input per dictionary is capped at this many bytes, sampled evenly.
const TRAIN_BYTES: usize = 16 << 20;

pub fn read(file: &File, e: &Entry) -> Vec<u8> {
    let mut buf = vec![0; e.len as usize];
    read_at(file, &mut buf, e.offset).expect("read tile store");
    buf
}

/// Trains one dictionary per (layer, zoom band) from the payloads in `file`, sampled evenly
/// in `entries` order. A group too small to train on gets an empty one.
pub fn train(file: &File, entries: &[Entry]) -> Dictionaries {
    let mut groups: BTreeMap<(u8, u8), Vec<&Entry>> = BTreeMap::new();
    for e in entries {
        groups.entry((e.layer, zoom_band(e.z))).or_default().push(e);
    }
    let trained: Vec<((u8, u8), Vec<u8>)> = groups
        .par_iter()
        .map(|(&k, es)| {
            let total: u64 = es.iter().map(|e| u64::from(e.len)).sum();
            let step = (total as usize / TRAIN_BYTES).max(1);
            let samples: Vec<Vec<u8>> = es.iter().step_by(step).map(|e| read(file, e)).collect();
            let dict = if total >= 128 << 10 && samples.len() >= 64 {
                zstd::dict::from_samples(&samples, DICT_SIZE).unwrap_or_default()
            } else {
                Vec::new()
            };
            (k, dict)
        })
        .collect();
    Dictionaries(trained.into_iter().collect())
}
