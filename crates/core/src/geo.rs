//! World coordinates (`docs/format.md`, Conventions): Web Mercator on a `u32` grid, y south.

/// A point in world coordinates.
pub type P = [u32; 2];

/// World units per axis: one unit is under 1 cm at the equator.
pub const WORLD: f64 = 4_294_967_296.0;
/// The Earth's circumference at the equator in Web Mercator, metres.
pub const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
/// Mean Earth radius, metres, for great-circle distances.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;
const MAX_LAT: f64 = 85.051_128_779_806_59;

/// A world point scaled to `0..1` on both axes, y south: the app's `f64` form of [`P`].
pub type Unit = [f64; 2];

pub fn lonlat_to_unit(lon: f64, lat: f64) -> Unit {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0;
    [x, y]
}

/// `[lon, lat]` in degrees.
pub fn unit_to_lonlat(u: Unit) -> [f64; 2] {
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * u[1]))
        .sinh()
        .atan()
        .to_degrees();
    [u[0] * 360.0 - 180.0, lat]
}

pub fn to_unit(p: P) -> Unit {
    [f64::from(p[0]) / WORLD, f64::from(p[1]) / WORLD]
}

/// Rounded to the nearest world unit, clamped to the world.
pub fn from_unit(u: Unit) -> P {
    u.map(|v| (v * WORLD).round().clamp(0.0, f64::from(u32::MAX)) as u32)
}

pub fn project(lat: f64, lon: f64) -> P {
    from_unit(lonlat_to_unit(lon, lat))
}

/// [`project`] of OSM's 10⁻⁷ degree coordinates.
pub fn project_e7(lat: i32, lon: i32) -> P {
    project(f64::from(lat) * 1e-7, f64::from(lon) * 1e-7)
}

/// `(lat, lon)` in degrees.
pub fn unproject(p: P) -> (f64, f64) {
    let [lon, lat] = unit_to_lonlat(to_unit(p));
    (lat, lon)
}

/// Metres per world unit at the latitude of `p`.
pub fn metres_per_unit(p: P) -> f64 {
    EARTH_CIRCUMFERENCE_M / WORLD * unproject(p).0.to_radians().cos()
}

/// Great-circle distance between two unit points, in metres.
pub fn distance_m(a: Unit, b: Unit) -> f64 {
    let [lon1, lat1] = unit_to_lonlat(a).map(f64::to_radians);
    let [lon2, lat2] = unit_to_lonlat(b).map(f64::to_radians);
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// Distance in world units.
pub fn dist(a: P, b: P) -> f64 {
    (f64::from(b[0]) - f64::from(a[0])).hypot(f64::from(b[1]) - f64::from(a[1]))
}

/// The Morton code of a point: x bits even, y bits odd.
pub fn morton(p: P) -> u64 {
    let spread = |v: u32| {
        let mut v = u64::from(v);
        v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
        v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333_3333_3333;
        (v | (v << 1)) & 0x5555_5555_5555_5555
    };
    spread(p[0]) | (spread(p[1]) << 1)
}

/// Morton code ranges, inclusive and ascending, whose union covers `bbox` (`min_x, min_y,
/// max_x, max_y`): at most nine aligned squares about the bbox's size, each one range.
pub fn morton_ranges(bbox: [u32; 4]) -> Vec<(u64, u64)> {
    let span = u64::from((bbox[2] - bbox[0]).max(bbox[3] - bbox[1])) + 1;
    // Squares of side 2^shift, the smallest power of two under the span, so the bbox
    // meets at most three in each direction.
    let shift = (64 - span.leading_zeros()).saturating_sub(1).min(32);
    if shift >= 31 {
        return vec![(0, u64::MAX)];
    }
    let mut out: Vec<(u64, u64)> = Vec::new();
    for y in bbox[1] >> shift..=bbox[3] >> shift {
        for x in bbox[0] >> shift..=bbox[2] >> shift {
            let lo = morton([x << shift, y << shift]);
            out.push((lo, lo + ((1u64 << (2 * shift)) - 1)));
        }
    }
    out.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(out.len());
    for (lo, hi) in out {
        match merged.last_mut() {
            Some(last) if last.1.checked_add(1) == Some(lo) => last.1 = hi,
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

/// A square bbox of half side `radius_m` around `p`, clamped to the world.
pub fn bbox_around(p: P, radius_m: f64) -> [u32; 4] {
    let r = (radius_m / metres_per_unit(p))
        .ceil()
        .min(f64::from(u32::MAX)) as u32;
    [
        p[0].saturating_sub(r),
        p[1].saturating_sub(r),
        p[0].saturating_add(r),
        p[1].saturating_add(r),
    ]
}

/// Whether `p` is inside an odd number of `rings` (each implicitly closed).
pub fn rings_contain(rings: &[Vec<P>], p: P) -> bool {
    let (px, py) = (f64::from(p[0]), f64::from(p[1]));
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let (ay, by) = (f64::from(a[1]), f64::from(b[1]));
            if (ay > py) != (by > py) {
                let (ax, bx) = (f64::from(a[0]), f64::from(b[0]));
                if ax + (py - ay) / (by - ay) * (bx - ax) > px {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips() {
        for (lat, lon) in [(0.0, 0.0), (39.96, -83.0), (58.45, -78.1), (-33.9, 151.2)] {
            let (la, lo) = unproject(project(lat, lon));
            assert!((la - lat).abs() < 1e-6 && (lo - lon).abs() < 1e-6);
        }
        // About 0.0093 m per unit at the equator, half that at 60°.
        let e = metres_per_unit(project(0.0, 0.0));
        assert!((e - 0.009_330).abs() < 1e-5, "{e}");
        assert!((metres_per_unit(project(60.0, 0.0)) / e - 0.5).abs() < 1e-3);
        for (lon, lat) in [(0.0, 0.0), (-81.7, 41.5), (-123.1, 49.3), (179.9, -60.0)] {
            let [lo, la] = unit_to_lonlat(lonlat_to_unit(lon, lat));
            assert!(
                (lo - lon).abs() < 1e-9 && (la - lat).abs() < 1e-9,
                "{lon},{lat}"
            );
        }
        assert!((lonlat_to_unit(0.0, 0.0)[1] - 0.5).abs() < 1e-9);
        assert!(lonlat_to_unit(0.0, 89.0)[1] >= 0.0);
        assert_eq!(from_unit(to_unit([123, u32::MAX])), [123, u32::MAX]);
        assert_eq!(morton([0b11, 0b01]), 0b0111);
    }

    #[test]
    fn morton_ranges_cover_the_bbox() {
        for bbox in [
            [1000, 2000, 1500, 2100],
            [0, 0, 0, 0],
            [65_535, 65_535, 65_536, 65_536],
            [123_456_789, 987_654_321, 123_999_999, 987_999_999],
        ] {
            let ranges = morton_ranges(bbox);
            assert!(ranges.len() <= 9, "{bbox:?}: {ranges:?}");
            assert!(ranges.windows(2).all(|w| w[0].1 < w[1].0));
            let step = |a: u32, b: u32| ((b - a) / 7).max(1);
            for x in (bbox[0]..=bbox[2]).step_by(step(bbox[0], bbox[2]) as usize) {
                for y in (bbox[1]..=bbox[3]).step_by(step(bbox[1], bbox[3]) as usize) {
                    let m = morton([x, y]);
                    assert!(ranges.iter().any(|r| r.0 <= m && m <= r.1), "{x},{y}");
                }
            }
        }
        assert_eq!(morton_ranges([0, 0, u32::MAX, 9]), [(0, u64::MAX)]);
    }

    #[test]
    fn even_odd_rings() {
        let sq = |a: u32, b: u32| vec![[a, a], [b, a], [b, b], [a, b]];
        let rings = [sq(0, 100), sq(40, 60)];
        assert!(rings_contain(&rings, [20, 20]));
        assert!(!rings_contain(&rings, [50, 50]), "in the hole");
        assert!(!rings_contain(&rings, [150, 50]));
    }
}
