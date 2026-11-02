//! Ocean polygons from osmdata.openstreetmap.de `water-polygons-split-4326.zip`, read
//! straight from the zip and clipped to the region's bounding box, optionally keeping only
//! the pieces that meet the region's polygon.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::geom::{self, BBox, Geometry, P, Rect, ring_area};
use crate::pbf::features::Feature;
use crate::poly::Poly;

/// Reads every polygon whose bbox meets `bbox` (and that meets `clip`, if given) and returns
/// it as a `layer`/`class` feature, with the number of records scanned and of pieces dropped
/// by `clip`.
pub fn read(
    zip_path: &Path,
    bbox: BBox,
    clip: Option<&Poly>,
    layer: u8,
    class: u16,
) -> Result<(Vec<Feature>, u64, u64)> {
    let file = File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))?;
    let name = zip
        .file_names()
        .find_map(|n| {
            n.ok()
                .filter(|n| n.ends_with(".shp"))
                .map(|n| n.into_owned())
        })
        .context("no .shp in water polygons zip")?;
    let entry = zip.by_name(&name)?;
    // What is left of the `.shp` to read, which bounds a record's length.
    let mut left = entry.size().saturating_sub(100);
    let mut shp = BufReader::with_capacity(1 << 20, entry);
    let (nw_lat, nw_lon) = geom::unproject(bbox.min);
    let (se_lat, se_lon) = geom::unproject(bbox.max);
    let rect: Rect = [
        i64::from(bbox.min[0]),
        i64::from(bbox.min[1]),
        i64::from(bbox.max[0]),
        i64::from(bbox.max[1]),
    ];

    let damaged = || {
        format!(
            "{}: damaged ({name}); run `fetch` again",
            zip_path.display()
        )
    };
    let mut header = [0u8; 100];
    shp.read_exact(&mut header).with_context(damaged)?;
    if i32::from_be_bytes(header[0..4].try_into()?) != 9994 {
        bail!("{name} is not a shapefile");
    }
    let mut features = Vec::new();
    let mut scanned = 0u64;
    let mut outside = 0u64;
    let mut rec = [0u8; 8];
    let mut buf = Vec::new();
    loop {
        match shp.read_exact(&mut rec) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        scanned += 1;
        let record = || format!("{}, record {scanned}", damaged());
        let len =
            u64::try_from(i32::from_be_bytes(rec[4..8].try_into()?)).with_context(record)? * 2;
        left = left.saturating_sub(8);
        ensure!(len <= left, "{}: longer than the file", record());
        left -= len;
        buf.resize(len as usize, 0);
        shp.read_exact(&mut buf).with_context(record)?;
        if Shp(&buf).i32_at(0).with_context(record)? != 5 {
            continue;
        }
        let b = |o| Shp(&buf).f64_at(o).with_context(record);
        let (xmin, ymin, xmax, ymax) = (b(4)?, b(12)?, b(20)?, b(28)?);
        if xmax < nw_lon || xmin > se_lon || ymax < se_lat || ymin > nw_lat {
            continue;
        }
        let mut rings = polygon(&buf).with_context(record)?;
        orient_rings(&mut rings);
        if clip.is_some_and(|c| !rings.iter().any(|r| c.meets(r))) {
            outside += 1;
            continue;
        }
        if let Some(g) = geom::clip(&Geometry::Polygon(rings), &rect) {
            features.push(Feature {
                id: scanned as i64 * 4 + 3,
                layer,
                class,
                geom: g,
                props: Vec::new(),
                minzoom: None,
            });
        }
    }
    Ok((features, scanned, outside))
}

/// A shapefile record's bytes, read with bounds checks.
struct Shp<'a>(&'a [u8]);

impl Shp<'_> {
    fn i32_at(&self, o: usize) -> Result<i32> {
        let b = self.0.get(o..o + 4).context("record too short")?;
        Ok(i32::from_le_bytes(b.try_into()?))
    }

    fn f64_at(&self, o: usize) -> Result<f64> {
        let b = self.0.get(o..o + 8).context("record too short")?;
        Ok(f64::from_le_bytes(b.try_into()?))
    }

    /// A count or index at `o`, which must not be negative.
    fn usize_at(&self, o: usize) -> Result<usize> {
        usize::try_from(self.i32_at(o)?).context("negative count")
    }
}

/// The rings of a polygon record (shape type 5), each without its repeated first point;
/// rings under three points are left out.
fn polygon(buf: &[u8]) -> Result<Vec<Vec<P>>> {
    let r = Shp(buf);
    let (parts, points) = (r.usize_at(36)?, r.usize_at(40)?);
    let pts_at = parts
        .checked_mul(4)
        .and_then(|n| n.checked_add(44))
        .context("too many parts")?;
    let end = points
        .checked_mul(16)
        .and_then(|n| n.checked_add(pts_at))
        .context("too many points")?;
    ensure!(
        end <= buf.len(),
        "{parts} parts and {points} points do not fit the record"
    );
    let starts = (0..parts)
        .map(|i| r.usize_at(44 + 4 * i))
        .collect::<Result<Vec<_>>>()?;
    let mut rings: Vec<Vec<P>> = Vec::with_capacity(parts);
    for (i, &s) in starts.iter().enumerate() {
        let e = starts.get(i + 1).copied().unwrap_or(points);
        ensure!(
            s <= e && e <= points,
            "part {i} runs from point {s} to {e} of {points}"
        );
        let mut ring = (s..e)
            .map(|k| {
                Ok(geom::project(
                    r.f64_at(pts_at + 16 * k + 8)?,
                    r.f64_at(pts_at + 16 * k)?,
                ))
            })
            .collect::<Result<Vec<P>>>()?;
        ring.pop(); // shapefile rings repeat the first point
        if ring.len() >= 3 {
            rings.push(ring);
        }
    }
    Ok(rings)
}

/// Orients one ocean piece as ours are: the outer ring clockwise on screen (y down) and the
/// holes counter-clockwise. The outer ring is the largest. The shapefile's own winding is not
/// relied on: `water-polygons-split` pieces come wound the other way from the shapefile
/// convention, and a piece whose only ring looked like a hole was dropped as all holes.
fn orient_rings(rings: &mut [Vec<P>]) {
    let largest = rings
        .iter()
        .map(|r| ring_area(r))
        .max_by(|a, b| a.abs().total_cmp(&b.abs()));
    if largest.is_some_and(|a| a < 0.0) {
        for r in rings.iter_mut() {
            r.reverse();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_are_oriented_by_their_largest_ring() {
        // Counter-clockwise on screen, with a clockwise island: the shapefiles' winding.
        let outer: Vec<P> = vec![[0, 0], [0, 100], [100, 100], [100, 0]];
        let island: Vec<P> = vec![[40, 40], [60, 40], [60, 60], [40, 60]];
        assert!(ring_area(&outer) < 0.0 && ring_area(&island) > 0.0);
        let mut rings = vec![outer.clone(), island];
        orient_rings(&mut rings);
        assert!(ring_area(&rings[0]) > 0.0 && ring_area(&rings[1]) < 0.0);
        // Already right: left alone. A lone square becomes an outer ring.
        let before = rings.clone();
        orient_rings(&mut rings);
        assert_eq!(rings, before);
        let mut lone = vec![outer];
        orient_rings(&mut lone);
        assert!(ring_area(&lone[0]) > 0.0);
    }

    /// A polygon record: shape type 5, a bbox, then `parts` and `points` as given, the part
    /// starts and the points.
    fn record(parts: i32, points: i32, starts: &[i32], pts: &[[f64; 2]]) -> Vec<u8> {
        let mut b = 5i32.to_le_bytes().to_vec();
        b.extend([0u8; 32]);
        b.extend(parts.to_le_bytes());
        b.extend(points.to_le_bytes());
        for s in starts {
            b.extend(s.to_le_bytes());
        }
        for [x, y] in pts {
            b.extend(x.to_le_bytes());
            b.extend(y.to_le_bytes());
        }
        b
    }

    #[test]
    fn damaged_polygons_are_errors() {
        let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]];
        let rings = polygon(&record(1, 5, &[0], &square)).unwrap();
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 4);
        // More points than the record holds, a negative count, a part past the points, parts
        // out of order, a record cut short: each an error, none a panic.
        assert!(polygon(&record(1, 6, &[0], &square)).is_err());
        assert!(polygon(&record(-1, 5, &[], &square)).is_err());
        assert!(polygon(&record(1, 5, &[7], &square)).is_err());
        assert!(polygon(&record(2, 5, &[3, 1], &square)).is_err());
        assert!(polygon(&record(i32::MAX, i32::MAX, &[0], &square)).is_err());
        assert!(polygon(&record(1, 5, &[0], &square)[..30]).is_err());
    }
}
