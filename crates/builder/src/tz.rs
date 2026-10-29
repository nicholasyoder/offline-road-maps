//! `TZON` for `build` (phase 4 step 4.5): timezone-boundary-builder's zone polygons, read
//! straight from its GeoJSON zip, kept where they meet the region (its `.poly` if the build
//! has one, else its bounding box), simplified to about 100 m and clipped to the box. The encoding and the reader are `omp_core::tz`'s.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::admin::index::simplify_rings;
use crate::geom::{self, Geometry, P, Rect, project};
use crate::poly::Poly;

pub use omp_core::tz::TZON;
/// Douglas–Peucker tolerance: zone borders run along counties and rivers, where 100 m is
/// far below what a route's day ends need.
pub const TOLERANCE_M: f64 = 100.0;
const LEVEL: i32 = 19;
/// How far past the region's bounding box the zones are kept, in degrees: so a point on
/// the box's edge still falls inside its zone.
const MARGIN_DEG: f64 = 0.05;

#[derive(Deserialize)]
struct Collection {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    properties: Properties,
    geometry: Shape,
}

#[derive(Deserialize)]
struct Properties {
    tzid: String,
}

/// Rings of `[lon, lat]`, closed (the first point repeated at the end).
type Rings = Vec<Vec<[f64; 2]>>;

#[derive(Deserialize)]
#[serde(tag = "type", content = "coordinates")]
enum Shape {
    Polygon(Rings),
    MultiPolygon(Vec<Rings>),
}

#[derive(Default)]
pub struct Sizes {
    pub zones: usize,
    pub vertices: usize,
    pub bytes: usize,
}

/// The zones of `zip` that meet `bounds` (west, south, east, north) and `clip`, each as its
/// rings in world coordinates, simplified and clipped to the bounds, sorted by name. A
/// polygon meets `clip` when a vertex of either is inside the other.
pub fn read(
    zip: &Path,
    bounds: [f64; 4],
    clip: Option<&Poly>,
) -> Result<Vec<(String, Vec<Vec<P>>)>> {
    let file = File::open(zip).with_context(|| format!("open {}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))?;
    let name = archive
        .file_names()
        .find_map(|n| {
            n.ok()
                .filter(|n| n.ends_with(".json"))
                .map(|n| n.into_owned())
        })
        .context("no .json in the time zone zip")?;
    let collection: Collection =
        serde_json::from_reader(BufReader::with_capacity(1 << 20, archive.by_name(&name)?))
            .with_context(|| format!("parse {name}"))?;
    let [w, s, e, n] = [
        bounds[0] - MARGIN_DEG,
        bounds[1] - MARGIN_DEG,
        bounds[2] + MARGIN_DEG,
        bounds[3] + MARGIN_DEG,
    ];
    let (nw, se) = (
        project(n.min(85.0), w.max(-180.0)),
        project(s.max(-85.0), e.min(180.0)),
    );
    let rect: Rect = [
        i64::from(nw[0]),
        i64::from(nw[1]),
        i64::from(se[0]),
        i64::from(se[1]),
    ];
    let mut zones: Vec<(String, Vec<Vec<P>>)> = collection
        .features
        .into_iter()
        .filter_map(|f| {
            let polygons = match f.geometry {
                Shape::Polygon(p) => vec![p],
                Shape::MultiPolygon(ps) => ps,
            };
            let mut rings = Vec::new();
            for polygon in polygons {
                // By the outer ring's bbox: a polygon can cover the box with no point in it.
                let Some(outer) = polygon.first() else {
                    continue;
                };
                let mut b = [180.0f64, 90.0, -180.0, -90.0];
                for &[lon, lat] in outer {
                    b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
                }
                let overlaps = b[0] <= e && b[2] >= w && b[1] <= n && b[3] >= s;
                if !overlaps {
                    continue;
                }
                let projected: Vec<Vec<P>> = polygon
                    .iter()
                    .enumerate()
                    .filter_map(|(k, r)| {
                        let pts: Vec<P> = r
                            .iter()
                            .map(|&[lon, lat]| project(lat.clamp(-85.0, 85.0), lon))
                            .collect();
                        geom::closed_way_ring(&pts, k == 0)
                    })
                    .flat_map(|r| simplify_rings(&[r], TOLERANCE_M))
                    .collect();
                if let Some(c) = clip
                    && !projected.iter().flatten().any(|&p| c.contains(p))
                    && !c
                        .points()
                        .any(|p| omp_core::geo::rings_contain(&projected, p))
                {
                    continue;
                }
                if let Some(Geometry::Polygon(clipped)) =
                    geom::clip(&Geometry::Polygon(projected), &rect)
                {
                    rings.extend(clipped);
                }
            }
            (!rings.is_empty()).then_some((f.properties.tzid, rings))
        })
        .collect();
    zones.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(zones)
}

/// `TZON`'s bytes and sizes.
pub fn section(zones: &[(String, Vec<Vec<P>>)]) -> Result<(Vec<u8>, Sizes)> {
    let bytes = omp_core::tz::encode(zones, LEVEL)?;
    let sizes = Sizes {
        zones: zones.len(),
        vertices: zones.iter().flat_map(|z| &z.1).map(Vec::len).sum(),
        bytes: bytes.len(),
    };
    Ok((bytes, sizes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omp_core::tz::Zones;

    #[test]
    fn reads_clips_and_encodes() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("tz.zip");
        // Two zones split at 87° W, a third far away, and a fourth whose outer ring holds
        // the box with no point in it.
        let json = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"tzid":"America/New_York"},"geometry":{"type":"Polygon",
             "coordinates":[[[-87,30],[-75,30],[-75,40],[-87,40],[-87,30]]]}},
            {"type":"Feature","properties":{"tzid":"America/Chicago"},"geometry":{"type":"MultiPolygon",
             "coordinates":[[[[-95,30],[-87,30],[-87,40],[-95,40],[-95,30]]]]}},
            {"type":"Feature","properties":{"tzid":"Europe/Paris"},"geometry":{"type":"Polygon",
             "coordinates":[[[0,45],[5,45],[5,50],[0,50],[0,45]]]}},
            {"type":"Feature","properties":{"tzid":"America/Toronto"},"geometry":{"type":"Polygon",
             "coordinates":[[[-120,20],[-60,20],[-60,60],[-120,60],[-120,20]]]}}
        ]}"#;
        let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file("combined.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut w, json.as_bytes()).unwrap();
        w.finish().unwrap();

        let zones = read(&zip_path, [-90.0, 32.0, -85.0, 36.0], None).unwrap();
        let names: Vec<&str> = zones.iter().map(|z| z.0.as_str()).collect();
        assert_eq!(
            names,
            ["America/Chicago", "America/New_York", "America/Toronto"]
        );
        let (bytes, sizes) = section(&zones).unwrap();
        assert_eq!(sizes.zones, 3);
        let z = Zones::decode(&bytes).unwrap();
        let at = |lat: f64, lon: f64| z.at(project(lat, lon)).map(|i| z.zone(i).name.as_str());
        assert_eq!(at(34.0, -88.0), Some("America/Chicago"));
        assert_eq!(at(34.0, -86.0), Some("America/New_York"));
        // Clipped to the box, and its margin.
        assert_eq!(at(34.0, -80.0), None);
    }
}
