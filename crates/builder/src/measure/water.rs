//! Ocean polygons from osmdata.openstreetmap.de `water-polygons-split-4326.zip`, read
//! straight from the zip and clipped to the region's bounding box.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::geom::{self, BBox, Geometry, P, Rect};
use super::osm::Feature;

/// Reads every polygon whose bbox meets `bbox` and returns it as a `layer`/`class` feature.
pub fn read(zip_path: &Path, bbox: BBox, layer: u8, class: u16) -> Result<(Vec<Feature>, u64)> {
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
    let mut shp = BufReader::with_capacity(1 << 20, zip.by_name(&name)?);
    let (nw_lat, nw_lon) = geom::unproject(bbox.min);
    let (se_lat, se_lon) = geom::unproject(bbox.max);
    let rect: Rect = [
        i64::from(bbox.min[0]),
        i64::from(bbox.min[1]),
        i64::from(bbox.max[0]),
        i64::from(bbox.max[1]),
    ];

    let mut header = [0u8; 100];
    shp.read_exact(&mut header)?;
    if i32::from_be_bytes(header[0..4].try_into()?) != 9994 {
        bail!("{name} is not a shapefile");
    }
    let mut features = Vec::new();
    let mut scanned = 0u64;
    let mut rec = [0u8; 8];
    let mut buf = Vec::new();
    loop {
        match shp.read_exact(&mut rec) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let words = i32::from_be_bytes(rec[4..8].try_into()?) as usize;
        buf.resize(words * 2, 0);
        shp.read_exact(&mut buf)?;
        scanned += 1;
        let i32_at = |o: usize| i32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
        let f64_at = |o: usize| f64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
        if i32_at(0) != 5 {
            continue;
        }
        let (xmin, ymin, xmax, ymax) = (f64_at(4), f64_at(12), f64_at(20), f64_at(28));
        if xmax < nw_lon || xmin > se_lon || ymax < se_lat || ymin > nw_lat {
            continue;
        }
        let parts = i32_at(36) as usize;
        let points = i32_at(40) as usize;
        let starts: Vec<usize> = (0..parts).map(|i| i32_at(44 + 4 * i) as usize).collect();
        let pts_at = 44 + 4 * parts;
        let mut rings: Vec<Vec<P>> = Vec::with_capacity(parts);
        for (i, &s) in starts.iter().enumerate() {
            let e = starts.get(i + 1).copied().unwrap_or(points);
            let mut ring: Vec<P> = (s..e)
                .map(|k| geom::project(f64_at(pts_at + 16 * k + 8), f64_at(pts_at + 16 * k)))
                .collect();
            ring.pop(); // shapefile rings repeat the first point
            // Shapefile outer rings are clockwise with y up; ours are clockwise with y down.
            ring.reverse();
            if ring.len() >= 3 {
                rings.push(ring);
            }
        }
        if let Some(g) = geom::clip(&Geometry::Polygon(rings), &rect) {
            features.push(Feature {
                id: scanned as i64 * 4 + 3,
                layer,
                class,
                geom: g,
                props: Vec::new(),
            });
        }
    }
    Ok((features, scanned))
}
