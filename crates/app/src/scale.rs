//! The scale bar (phase1-plan.md 1.9): its units, and a round length that fits.

use serde::{Deserialize, Serialize};

/// The Earth's circumference at the equator in Web Mercator, metres.
const CIRCUMFERENCE: f64 = 40_075_016.686;
const FOOT: f64 = 0.3048;
const MILE: f64 = 1609.344;

/// The scale bar's units; saved in `config.toml`, else they follow the pack
/// ([`crate::packs::default_units`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    Miles,
    Kilometres,
    /// Two bars: miles above, kilometres below.
    Both,
}

impl Units {
    pub const ALL: [Units; 3] = [Units::Miles, Units::Kilometres, Units::Both];

    pub fn name(self) -> &'static str {
        match self {
            Units::Miles => "Miles",
            Units::Kilometres => "Kilometres",
            Units::Both => "Both",
        }
    }
}

/// Metres per point at latitude `lat` (degrees), for a camera `scale` in points per world
/// unit.
pub fn metres_per_point(lat: f64, scale: f64) -> f64 {
    CIRCUMFERENCE * lat.to_radians().cos() / scale
}

/// The longest round length no longer than `max` points, as (its length in points, its
/// label): 1, 2 or 5 times a power of ten, in metres and kilometres, or in feet and miles
/// (feet below a tenth of a mile).
pub fn bar(metres_per_point: f64, max: f64, imperial: bool) -> (f64, String) {
    let max_m = metres_per_point * max;
    let (unit, name, n) = if !imperial {
        match round_down(max_m) {
            n if n >= 1000.0 => (1000.0, "km", n / 1000.0),
            n => (1.0, "m", n),
        }
    } else if max_m >= 0.1 * MILE {
        (MILE, "mi", round_down(max_m / MILE))
    } else {
        (FOOT, "ft", round_down(max_m / FOOT))
    };
    let text = if n >= 1.0 {
        format!("{n:.0} {name}")
    } else {
        format!("{n} {name}")
    };
    (n * unit / metres_per_point, text)
}

/// The largest 1, 2 or 5 times a power of ten that is at most `v`.
fn round_down(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 0.0;
    }
    let p = 10f64.powf(v.log10().floor());
    let m = [5.0, 2.0, 1.0]
        .into_iter()
        .find(|m| m * p <= v * (1.0 + 1e-9));
    // Rounded so that 0.1 * 2 shows as 0.2.
    let n = m.unwrap_or(1.0) * p;
    let digits = (-p.log10()).max(0.0) as i32;
    let f = 10f64.powi(digits);
    (n * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_down_to_1_2_5() {
        assert_eq!(round_down(7.3), 5.0);
        assert_eq!(round_down(1999.0), 1000.0);
        assert_eq!(round_down(2000.0), 2000.0);
        assert_eq!(round_down(0.31), 0.2);
        assert_eq!(round_down(0.0), 0.0);
    }

    #[test]
    fn bars_fit_and_read_well() {
        // 10 m per point, 100 points at most: 1 km, 100 points.
        let (px, t) = bar(10.0, 100.0, false);
        assert_eq!(t, "1 km");
        assert!((px - 100.0).abs() < 1e-9);
        let (px, t) = bar(3.0, 100.0, false);
        assert_eq!(t, "200 m");
        assert!(px <= 100.0);
        assert_eq!(bar(3.0, 100.0, true).1, "0.1 mi");
        // 150 m: under a tenth of a mile, so feet.
        assert_eq!(bar(1.5, 100.0, true).1, "200 ft");
        assert_eq!(bar(20.0, 100.0, true).1, "1 mi");
        assert_eq!(bar(5.0, 100.0, true).1, "0.2 mi");
        assert_eq!(bar(5000.0, 120.0, true).1, "200 mi");
        for mpp in [0.05, 1.0, 37.0, 900.0, 60_000.0] {
            for imperial in [false, true] {
                let (px, _) = bar(mpp, 120.0, imperial);
                assert!(
                    px > 120.0 / 5.1 && px <= 120.0 + 1e-6,
                    "{mpp} {imperial}: {px}"
                );
            }
        }
    }

    #[test]
    fn metres_per_point_shrinks_towards_the_poles() {
        let s = 512.0 * 2f64.powi(10);
        let eq = metres_per_point(0.0, s);
        assert!((eq - 76.437).abs() < 0.01, "{eq}");
        assert!((metres_per_point(60.0, s) - eq / 2.0).abs() < 1e-6);
    }
}
