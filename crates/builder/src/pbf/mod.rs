//! Reading OSM PBF files: one parallel pass over the data blocks ([`Pbf::pass`]), the
//! header's bounds, the needed-node stores ([`nodes`]) and the whitelisted feature reader
//! ([`features`]).

pub mod features;
pub mod nodes;

use std::collections::HashMap;
use std::ops::BitOr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use osmpbf::{Blob, BlobDecode, BlobReader, BlobType, ByteOffset, HeaderBlock, PrimitiveBlock};
use rayon::prelude::*;

/// The element types a data block holds, as a set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Kinds(u8);

impl Kinds {
    pub const NODES: Kinds = Kinds(1);
    pub const WAYS: Kinds = Kinds(2);
    pub const RELATIONS: Kinds = Kinds(4);

    fn of(block: &PrimitiveBlock) -> Kinds {
        let mut k = 0;
        for g in block.groups() {
            if g.dense_nodes().next().is_some() || g.nodes().next().is_some() {
                k |= Kinds::NODES.0;
            }
            if g.ways().next().is_some() {
                k |= Kinds::WAYS.0;
            }
            if g.relations().next().is_some() {
                k |= Kinds::RELATIONS.0;
            }
        }
        Kinds(k)
    }

    fn meets(self, o: Kinds) -> bool {
        self.0 & o.0 != 0
    }
}

impl BitOr for Kinds {
    type Output = Kinds;
    fn bitor(self, o: Kinds) -> Kinds {
        Kinds(self.0 | o.0)
    }
}

/// A PBF file, with the kinds of element each data blob holds once they are known, so that
/// a pass reads and decodes only the blobs of the kinds it asks for. Extracts are sorted
/// (nodes, then ways, then relations), so a relations pass reads a few percent of the file.
pub struct Pbf {
    path: PathBuf,
    /// `(offset, kinds)` of every data blob, in file order: from [`sorted_index`] when the
    /// header says the file is sorted, else from the first pass, which reads every blob.
    index: OnceLock<Vec<(u64, Kinds)>>,
}

impl Pbf {
    pub fn open(path: &Path) -> Result<Self> {
        let start = Instant::now();
        let index = OnceLock::new();
        match sorted_index(path).with_context(|| format!("index {}", path.display()))? {
            Some(i) => {
                let count = |k: Kinds| i.iter().filter(|e| e.1.meets(k)).count();
                eprintln!(
                    "  PBF: {} blobs: {} with nodes, {} with ways, {} with relations; {:.1} s",
                    i.len(),
                    count(Kinds::NODES),
                    count(Kinds::WAYS),
                    count(Kinds::RELATIONS),
                    start.elapsed().as_secs_f64()
                );
                let _ = index.set(i);
            }
            None => eprintln!("  PBF: not sorted by type; the first pass reads all of it"),
        }
        Ok(Pbf {
            path: path.to_owned(),
            index,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` in parallel over every data block that holds one of `kinds`, each into its
    /// own `init()`, and returns the outputs in no particular order. Blocks of other kinds
    /// may be passed too (all of them on the first pass), so `f` must skip elements it does
    /// not want. Prints the pass's time as `what`, with the CPU time spent decoding blobs
    /// apart from the time in `f`.
    pub fn pass<T: Send>(
        &self,
        what: &str,
        kinds: Kinds,
        init: impl Fn() -> T + Sync,
        f: impl Fn(&PrimitiveBlock, &mut T) -> Result<()> + Sync,
    ) -> Result<Vec<T>> {
        let start = Instant::now();
        let (blobs, decode_ns, work_ns) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
        let mut reader = BlobReader::seekable_from_path(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        let found = Mutex::new(Vec::new());
        let indexed = self.index.get();
        let blob_iter: Box<dyn Iterator<Item = Result<Blob>> + Send> = match indexed {
            Some(index) => {
                let mut last = None;
                Box::new(
                    index
                        .iter()
                        .enumerate()
                        .filter(move |(_, (_, k))| k.meets(kinds))
                        .map(move |(i, &(offset, _))| {
                            // Wanted blobs mostly follow each other: seek only over gaps.
                            if last.is_none_or(|l| l + 1 != i) {
                                reader.seek(ByteOffset(offset))?;
                            }
                            last = Some(i);
                            let blob = reader.next().context("PBF ended early")??;
                            ensure!(
                                blob.offset() == Some(ByteOffset(offset)),
                                "{} changed while it was read",
                                self.path.display()
                            );
                            Ok(blob)
                        }),
                )
            }
            None => Box::new(reader.map(|b| Ok(b?))),
        };
        let out = blob_iter
            .par_bridge()
            .map(|blob| {
                let mut out = init();
                let blob = blob?;
                let t = Instant::now();
                let decoded = blob.decode()?;
                let t2 = Instant::now();
                decode_ns.fetch_add((t2 - t).as_nanos() as u64, Relaxed);
                if let BlobDecode::OsmData(block) = decoded {
                    blobs.fetch_add(1, Relaxed);
                    if indexed.is_none() {
                        let offset = blob.offset().context("blob without an offset")?.0;
                        found.lock().unwrap().push((offset, Kinds::of(&block)));
                    }
                    f(&block, &mut out)?;
                }
                work_ns.fetch_add(t2.elapsed().as_nanos() as u64, Relaxed);
                Ok(out)
            })
            .collect::<Result<Vec<T>>>()?;
        if indexed.is_none() {
            let mut found = found.into_inner().unwrap();
            found.sort_unstable_by_key(|(offset, _)| *offset);
            let _ = self.index.set(found);
        }
        eprintln!(
            "    pass {what}: {} blobs, {:.1} s, decode {:.0} CPU s, work {:.0} CPU s",
            blobs.into_inner(),
            start.elapsed().as_secs_f64(),
            decode_ns.into_inner() as f64 / 1e9,
            work_ns.into_inner() as f64 / 1e9,
        );
        Ok(out)
    }
}

/// The PBF's header block.
fn header(path: &Path) -> Result<Option<HeaderBlock>> {
    let mut reader = BlobReader::from_path(path)?;
    if let Some(blob) = reader.next()
        && let BlobDecode::OsmHeader(h) = blob?.decode()?
    {
        return Ok(Some(*h));
    }
    Ok(None)
}

/// The PBF header's bounding box: west, south, east, north in degrees.
pub fn header_bounds(path: &Path) -> Result<Option<[f64; 4]>> {
    Ok(header(path)?
        .and_then(|h| h.bbox())
        .map(|b| [b.left, b.bottom, b.right, b.top]))
}

/// The index of a file whose header says it is sorted by type, then ID, from the blob
/// headers alone and a binary search for the first blob holding ways or relations and the
/// first holding relations: about 40 blobs decoded. Before the first, blobs hold only nodes;
/// between the two, only ways; after the second, only relations. `None` if the file is not
/// sorted, or has a blob of an unknown type or with no elements.
fn sorted_index(path: &Path) -> Result<Option<Vec<(u64, Kinds)>>> {
    let Some(h) = header(path)? else {
        return Ok(None);
    };
    if !h
        .optional_features()
        .iter()
        .any(|f| f == "Sort.Type_then_ID")
    {
        return Ok(None);
    }
    let mut reader = BlobReader::seekable_from_path(path)?;
    let mut offsets = Vec::new();
    while let Some(next) = reader.next_header_skip_blob() {
        let (header, offset) = next?;
        match header.blob_type() {
            BlobType::OsmHeader => {}
            BlobType::OsmData => offsets.push(offset.context("blob without an offset")?.0),
            BlobType::Unknown(_) => return Ok(None),
        }
    }
    let mut known: HashMap<usize, Kinds> = HashMap::new();
    let mut kinds_at = |i: usize| -> Result<Kinds> {
        if let Some(&k) = known.get(&i) {
            return Ok(k);
        }
        let k = match reader.blob_from_offset(ByteOffset(offsets[i]))?.decode()? {
            BlobDecode::OsmData(block) => Kinds::of(&block),
            _ => Kinds(0),
        };
        known.insert(i, k);
        Ok(k)
    };
    // The first blob in `lo..hi` where `kinds_at` meets `want`, or `hi`.
    let mut first = |want: Kinds, mut lo: usize, mut hi: usize| -> Result<Option<usize>> {
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let k = kinds_at(mid)?;
            if k.0 == 0 {
                return Ok(None);
            }
            match k.meets(want) {
                true => hi = mid,
                false => lo = mid + 1,
            }
        }
        Ok(Some(lo))
    };
    let n = offsets.len();
    let Some(ways) = first(Kinds::WAYS | Kinds::RELATIONS, 0, n)? else {
        return Ok(None);
    };
    let Some(rels) = first(Kinds::RELATIONS, ways, n)? else {
        return Ok(None);
    };
    let mut index = Vec::with_capacity(n);
    for (i, &offset) in offsets.iter().enumerate() {
        let k = match i {
            _ if i == ways || i == rels => kinds_at(i)?,
            _ if i < ways => Kinds::NODES,
            _ if i < rels => Kinds::WAYS,
            _ => Kinds::RELATIONS,
        };
        index.push((offset, k));
    }
    Ok(Some(index))
}

#[cfg(test)]
mod tests;
