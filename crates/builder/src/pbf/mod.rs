//! Reading OSM PBF files: one parallel pass over the data blocks ([`pass`]), the header's
//! bounds, the needed-node stores ([`nodes`]) and the whitelisted feature reader
//! ([`features`]).

pub mod features;
pub mod nodes;

use std::path::Path;

use anyhow::{Context, Result};
use osmpbf::{BlobDecode, BlobReader, PrimitiveBlock};
use rayon::prelude::*;

/// Runs `f` over every data block of the file in parallel, each into its own `init()`.
/// Returns the outputs in no particular order.
pub fn pass<T: Send>(
    path: &Path,
    init: impl Fn() -> T + Sync,
    f: impl Fn(&PrimitiveBlock, &mut T) -> Result<()> + Sync,
) -> Result<Vec<T>> {
    BlobReader::from_path(path)
        .with_context(|| format!("open {}", path.display()))?
        .par_bridge()
        .map(|blob| {
            let mut out = init();
            if let BlobDecode::OsmData(block) = blob?.decode()? {
                f(&block, &mut out)?;
            }
            Ok(out)
        })
        .collect()
}

/// The PBF header's bounding box: west, south, east, north in degrees.
pub fn header_bounds(path: &Path) -> Result<Option<[f64; 4]>> {
    let mut reader = BlobReader::from_path(path)?;
    if let Some(blob) = reader.next()
        && let BlobDecode::OsmHeader(h) = blob?.decode()?
    {
        return Ok(h.bbox().map(|b| [b.left, b.bottom, b.right, b.top]));
    }
    Ok(None)
}
