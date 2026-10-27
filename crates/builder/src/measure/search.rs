//! A ballpark search index: an `fst` over normalized name tokens and naive postings lists.

use std::collections::{BTreeMap, HashSet};

use omp_core::normalize;
use omp_core::whitelist::Whitelist;
use serde::Serialize;

use crate::geom::Geometry;
use crate::pbf::features::Feature;
use omp_core::varint::{len as varint_len, put as put_varint};

#[derive(Serialize)]
pub struct SearchStats {
    pub places: u64,
    pub pois: u64,
    /// Named road ways deduplicated by (name, z12 tile of their first point).
    pub streets: u64,
    pub tokens: u64,
    pub postings: u64,
    pub fst_bytes: u64,
    /// Delta + varint feature ids per token.
    pub postings_bytes: u64,
    /// Per entry: varint name length + name bytes + 8 bytes of coordinates.
    pub entries_bytes: u64,
}

pub fn build<'a>(
    features: impl IntoIterator<Item = &'a Feature>,
    wl: &Whitelist,
) -> anyhow::Result<SearchStats> {
    let layer = |n: &str| wl.layer_index(n).map(|i| i as u8);
    let (places, pois, roads) = (layer("places"), layer("pois"), layer("roads"));
    let name_prop = |l: u8| {
        wl.layers[l as usize]
            .props
            .iter()
            .position(|p| p == "name")
            .map(|i| i as u8)
    };
    let mut entries: Vec<(&str, [u32; 2])> = Vec::new();
    let (mut n_places, mut n_pois) = (0, 0);
    let mut streets = HashSet::new();
    for f in features {
        let l = Some(f.layer);
        if l != places && l != pois && l != roads {
            continue;
        }
        let Some(np) = name_prop(f.layer) else {
            continue;
        };
        let Some((_, name)) = f.props.iter().find(|(k, _)| *k == np) else {
            continue;
        };
        let p = match &f.geom {
            Geometry::Point(p) => *p,
            Geometry::Lines(parts) | Geometry::Polygon(parts) => parts[0][0],
        };
        if l == roads {
            if !streets.insert((&**name, p[0] >> 20, p[1] >> 20)) {
                continue;
            }
        } else if l == places {
            n_places += 1;
        } else {
            n_pois += 1;
        }
        entries.push((name, p));
    }

    let mut postings: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (i, (name, _)) in entries.iter().enumerate() {
        for t in normalize::tokens(name) {
            let list = postings.entry(t).or_default();
            if list.last() != Some(&(i as u32)) {
                list.push(i as u32);
            }
        }
    }
    let mut builder = fst::MapBuilder::memory();
    let mut post_bytes = Vec::new();
    let mut total = 0;
    for (token, ids) in &postings {
        builder.insert(token, post_bytes.len() as u64)?;
        put_varint(&mut post_bytes, ids.len() as u64);
        let mut prev = 0;
        for &id in ids {
            put_varint(&mut post_bytes, u64::from(id - prev));
            prev = id;
        }
        total += ids.len() as u64;
    }
    let fst_bytes = builder.into_inner()?.len() as u64;
    Ok(SearchStats {
        places: n_places,
        pois: n_pois,
        streets: streets.len() as u64,
        tokens: postings.len() as u64,
        postings: total,
        fst_bytes,
        postings_bytes: post_bytes.len() as u64,
        entries_bytes: entries
            .iter()
            .map(|(n, _)| varint_len(n.len() as u64) + n.len() as u64 + 8)
            .sum(),
    })
}
