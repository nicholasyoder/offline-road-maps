//! World coordinates (`docs/format.md`, Conventions): Web Mercator on a `u32` grid, y south.

/// A point in world coordinates.
pub type P = [u32; 2];

const WORLD: f64 = 4_294_967_296.0;
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
const MAX_LAT: f64 = 85.051_128_779_806_59;

pub fn project(lat: f64, lon: f64) -> P {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0;
    let w = |v: f64| (v * WORLD).round().clamp(0.0, f64::from(u32::MAX)) as u32;
    [w(x), w(y)]
}

/// `(lat, lon)` in degrees.
pub fn unproject(p: P) -> (f64, f64) {
    let x = f64::from(p[0]) / WORLD;
    let y = f64::from(p[1]) / WORLD;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y))
        .sinh()
        .atan()
        .to_degrees();
    (lat, x * 360.0 - 180.0)
}

/// Metres per world unit at the latitude of `p`.
pub fn metres_per_unit(p: P) -> f64 {
    EARTH_CIRCUMFERENCE_M / WORLD * unproject(p).0.to_radians().cos()
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
        assert_eq!(morton([0b11, 0b01]), 0b0111);
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
