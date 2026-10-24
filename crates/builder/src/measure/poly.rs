//! Osmosis `.poly` boundary files, as Geofabrik publishes for each extract. `addr-spike`
//! splits a continent's OSM addresses by state and province with them, and `measure` keeps
//! the ocean pieces that meet the extract's polygon.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::measure::geom::{self, BBox, P};

pub struct Poly {
    /// Rings of `(lon, lat)`; holes are rings too, since the test is even-odd.
    rings: Vec<Vec<(f64, f64)>>,
    /// `(min lon, min lat, max lon, max lat)`.
    bounds: (f64, f64, f64, f64),
}

impl Poly {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// The format: a name line, then sections of a name line (`!` for a hole), one
    /// `lon lat` pair per line and `END`, then a final `END`.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .skip(1);
        let mut rings = Vec::new();
        loop {
            match lines.next() {
                None => bail!("missing final END"),
                Some("END") => break,
                Some(_) => {}
            }
            let mut ring = Vec::new();
            loop {
                let line = lines.next().context("unterminated ring")?;
                if line == "END" {
                    break;
                }
                let mut it = line.split_whitespace().map(str::parse::<f64>);
                match (it.next(), it.next()) {
                    (Some(Ok(lon)), Some(Ok(lat))) => ring.push((lon, lat)),
                    _ => bail!("bad coordinate line {line:?}"),
                }
            }
            if ring.len() >= 3 {
                rings.push(ring);
            }
        }
        let mut bounds = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(lon, lat) in rings.iter().flatten() {
            bounds.0 = bounds.0.min(lon);
            bounds.1 = bounds.1.min(lat);
            bounds.2 = bounds.2.max(lon);
            bounds.3 = bounds.3.max(lat);
        }
        if rings.is_empty() {
            bail!("no rings");
        }
        Ok(Self { rings, bounds })
    }

    pub fn contains(&self, p: P) -> bool {
        let (lat, lon) = geom::unproject(p);
        let b = self.bounds;
        if lon < b.0 || lat < b.1 || lon > b.2 || lat > b.3 {
            return false;
        }
        let mut inside = false;
        for ring in &self.rings {
            let mut j = ring.len() - 1;
            for i in 0..ring.len() {
                let ((xi, yi), (xj, yj)) = (ring[i], ring[j]);
                if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
                    inside = !inside;
                }
                j = i;
            }
        }
        inside
    }

    /// Whether `ring` (projected) meets the polygon: a vertex of either lies inside the other's
    /// area or bounds. Edges crossing without either is possible but needs a ring larger than
    /// the polygon's gaps; good enough for ocean pieces against a region boundary.
    pub fn meets(&self, ring: &[P]) -> bool {
        if ring.iter().any(|&p| self.contains(p)) {
            return true;
        }
        let b = BBox::of(ring.iter().copied());
        let rect = [b.min[0], b.min[1], b.max[0], b.max[1]].map(i64::from);
        self.rings
            .iter()
            .flatten()
            .any(|&(lon, lat)| geom::bbox_inside(&BBox::of([geom::project(lat, lon)]), &rect))
    }

    /// The bounds, grown by `margin_deg` on every side, in projected coordinates.
    pub fn bbox(&self, margin_deg: f64) -> BBox {
        let (w, s, e, n) = self.bounds;
        BBox {
            min: geom::project(n + margin_deg, w - margin_deg),
            max: geom::project(s - margin_deg, e + margin_deg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE_WITH_HOLE: &str = "test
1
   0.0 0.0
   10.0 0.0
   10.0 10.0
   0.0 10.0
END
!2
   4.0 4.0
   6.0 4.0
   6.0 6.0
   4.0 6.0
END
END
";

    #[test]
    fn contains_with_hole() {
        let poly = Poly::parse(SQUARE_WITH_HOLE).unwrap();
        let at = |lon, lat| poly.contains(geom::project(lat, lon));
        assert!(at(1.0, 1.0));
        assert!(at(9.0, 5.0));
        assert!(!at(5.0, 5.0));
        assert!(!at(11.0, 5.0));
        assert!(!at(5.0, -1.0));
        let b = poly.bbox(1.0);
        assert!(geom::bbox_inside(
            &BBox::of([geom::project(10.5, -0.5)]),
            &[
                i64::from(b.min[0]),
                i64::from(b.min[1]),
                i64::from(b.max[0]),
                i64::from(b.max[1])
            ]
        ));
    }

    #[test]
    fn meets_rings() {
        let poly = Poly::parse(SQUARE_WITH_HOLE).unwrap();
        let ring = |pts: &[(f64, f64)]| -> Vec<P> {
            pts.iter()
                .map(|&(lon, lat)| geom::project(lat, lon))
                .collect()
        };
        // Overlapping one corner, around the whole polygon, and outside it.
        assert!(poly.meets(&ring(&[(9.0, 9.0), (12.0, 9.0), (12.0, 12.0)])));
        assert!(poly.meets(&ring(&[
            (-5.0, -5.0),
            (15.0, -5.0),
            (15.0, 15.0),
            (-5.0, 15.0)
        ])));
        assert!(!poly.meets(&ring(&[(20.0, 20.0), (21.0, 20.0), (21.0, 21.0)])));
    }

    #[test]
    fn rejects_truncated() {
        assert!(Poly::parse("x\n1\n 0 0\n 1 0\n").is_err());
    }
}
