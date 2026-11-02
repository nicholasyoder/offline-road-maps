//! The scale bar (phase1-plan.md 1.9): its units, a round length that fits, and the bar as
//! drawn over the window's map and on a printed page.

use eframe::egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use omp_core::geo;
use serde::{Deserialize, Serialize};

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
    geo::EARTH_CIRCUMFERENCE_M * lat.to_radians().cos() / scale
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

/// How the bar is drawn: its ink, and the card under it on a printed page (the window's sits
/// in a panel of its own).
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub ink: Color32,
    pub card: Option<Color32>,
}

/// The longest a bar is, in points.
const MAX: f64 = 120.0;
/// A bar's row, in points.
const ROW: f32 = 13.0;
/// The card's margin around the bars.
const CARD_MARGIN: Vec2 = Vec2::new(5.0, 3.0);

/// The bars for `units`, as (imperial, ticks up): miles with its ticks up, kilometres below
/// it with its ticks down.
fn bars(units: Units) -> &'static [(bool, bool)] {
    match units {
        Units::Miles => &[(true, true)],
        Units::Kilometres => &[(false, true)],
        Units::Both => &[(true, true), (false, false)],
    }
}

fn font() -> FontId {
    FontId::proportional(11.0)
}

/// The size [`paint`] takes, card included.
pub fn size(p: &Painter, metres_per_point: f64, units: Units, look: Look) -> Vec2 {
    let width = bars(units)
        .iter()
        .map(|&(imperial, _)| {
            let (len, text) = bar(metres_per_point, MAX, imperial);
            let label = p.layout_no_wrap(text, font(), look.ink);
            1.0 + len as f32 + 5.0 + label.size().x
        })
        .fold(0.0, f32::max);
    let bars = Vec2::new(width, ROW * bars(units).len() as f32);
    match look.card {
        Some(_) => bars + 2.0 * CARD_MARGIN,
        None => bars,
    }
}

/// Draws one bar per unit, each at most 120 points long, with the top-left corner at `at`;
/// returns the rect drawn, [`size`] big.
pub fn paint(p: &Painter, at: Pos2, metres_per_point: f64, units: Units, look: Look) -> Rect {
    let rect = Rect::from_min_size(at, size(p, metres_per_point, units, look));
    let mut top = rect.top();
    let mut left = rect.left();
    if let Some(card) = look.card {
        p.rect_filled(rect, CornerRadius::same(3), card);
        top += CARD_MARGIN.y;
        left += CARD_MARGIN.x;
    }
    let stroke = Stroke::new(1.5, look.ink);
    for &(imperial, up) in bars(units) {
        let (len, text) = bar(metres_per_point, MAX, imperial);
        let (x0, x1) = (left + 1.0, left + 1.0 + len as f32);
        let (y, tip) = match up {
            true => (top + ROW - 0.75, top + 3.0),
            false => (top + 0.75, top + ROW - 3.0),
        };
        p.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], stroke);
        p.line_segment([Pos2::new(x0, y), Pos2::new(x0, tip)], stroke);
        p.line_segment([Pos2::new(x1, y), Pos2::new(x1, tip)], stroke);
        p.text(
            Pos2::new(x1 + 5.0, top + ROW / 2.0),
            Align2::LEFT_CENTER,
            text,
            font(),
            look.ink,
        );
        top += ROW;
    }
    rect
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
