//! The search box (phase2-plan.md 2.7): suggestions as you type, the results list for
//! Enter, flying to a result and the marker left there.
//!
//! - [`worker`]: the thread that runs the queries over [`omp_core::find`], level by level,
//!   and answers "what's here"; no egui.
//! - [`session`]: which answer is the latest, the suggestions and results it gives, and the
//!   recent searches.
//! - [`ui`]: the box, its keys, the suggestions and the results panel.
//! - [`draw`]: the kind icons, a row, and the marker.

mod draw;
mod session;
mod ui;
mod worker;

use omp_core::find::{Hit, HitKind};
use omp_core::geo;

use crate::camera::{self, TILE_PX};
use crate::scale::Units;

pub use draw::{Icons, draw_marker, draw_pin};
pub use session::Taken;
pub use ui::{BenchKey, SearchUi};
pub use worker::Status;

/// The zoom an address is shown at.
const ADDRESS_ZOOM: f64 = 17.0;
/// The most a place or street is zoomed in to, and the least.
const FIT_ZOOM: [f64; 2] = [4.0, 17.0];

/// The view to fly to for a hit: an address at z17, anything else zoomed so its radius
/// takes 40% of the shorter side of the window.
pub fn view_for(hit: &Hit, size: camera::Vec2) -> (camera::Vec2, f64) {
    let w = geo::to_unit(hit.p);
    if hit.kind == HitKind::Address {
        return (w, ADDRESS_ZOOM);
    }
    let lat = geo::unit_to_lonlat(w)[1];
    let radius = hit.radius_m.max(50.0) / (geo::EARTH_CIRCUMFERENCE_M * lat.to_radians().cos());
    let side = size[0].min(size[1]).max(100.0);
    let zoom = (0.4 * side / (radius * TILE_PX)).log2();
    (w, zoom.clamp(FIT_ZOOM[0], FIT_ZOOM[1]))
}

/// "450 m", "3.2 km", "32 km"; "500 ft", "3.2 mi", "32 mi"; both for [`Units::Both`].
pub fn distance_text(m: f64, units: Units) -> String {
    let metric = || match m {
        m if m < 1000.0 => format!("{:.0} m", (m / 10.0).round() * 10.0),
        m if m < 10_000.0 => format!("{:.1} km", m / 1000.0),
        m => format!("{:.0} km", m / 1000.0),
    };
    let imperial = || match m / 1609.344 {
        mi if mi < 0.1 => format!("{:.0} ft", (m / 0.3048 / 10.0).round() * 10.0),
        mi if mi < 10.0 => format!("{mi:.1} mi"),
        mi => format!("{mi:.0} mi"),
    };
    match units {
        Units::Kilometres => metric(),
        Units::Miles => imperial(),
        Units::Both => format!("{} · {}", imperial(), metric()),
    }
}

/// Hits and answers for the tests of every part.
#[cfg(test)]
mod testing {
    use std::time::Duration;

    use omp_core::find::{Hit, HitKind};

    use super::worker::{Answer, Query};

    pub fn hit(kind: HitKind, lat: f64, lon: f64, radius_m: f64) -> Hit {
        Hit {
            kind,
            class: 0,
            name: "x".into(),
            context: String::new(),
            p: omp_core::geo::project(lat, lon),
            radius_m,
            approx: false,
            number_missing: false,
            postcode: None,
            units: Vec::new(),
            entry: None,
            street: None,
            score: 1.0,
            exact: true,
        }
    }

    pub fn answer(q: &Query, hits: Vec<Hit>) -> Answer {
        Answer {
            query: q.clone(),
            hits: Ok(hits),
            find: Duration::from_millis(3),
            level: 0,
            more: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::hit;
    use super::*;
    use omp_core::search::Kind;

    #[test]
    fn views_fit_results() {
        let size = [1280.0, 800.0];
        let (_, z) = view_for(&hit(HitKind::Address, 40.0, -83.0, 10.0), size);
        assert_eq!(z, ADDRESS_ZOOM);
        // A city 8 km across fills 40% of 800 points at about z11.
        let (_, z) = view_for(&hit(HitKind::Entry(Kind::Place), 40.0, -83.0, 8000.0), size);
        assert!((10.5..11.5).contains(&z), "{z}");
        let (_, z) = view_for(&hit(HitKind::Entry(Kind::Admin), 40.0, -83.0, 2e6), size);
        assert_eq!(z, FIT_ZOOM[0]);
        let (w, _) = view_for(&hit(HitKind::Entry(Kind::Street), 40.0, -83.0, 500.0), size);
        let [lon, lat] = geo::unit_to_lonlat(w);
        assert!((lat - 40.0).abs() < 1e-6 && (lon + 83.0).abs() < 1e-6);
    }

    #[test]
    fn distances() {
        let a = geo::lonlat_to_unit(-83.0, 40.0);
        let b = geo::lonlat_to_unit(-82.0, 40.0);
        // A degree of longitude at 40° N: 85.2 km on the great circle.
        assert!((geo::distance_m(a, b) - 85_180.0).abs() < 10.0);
        assert_eq!(distance_text(437.0, Units::Kilometres), "440 m");
        assert_eq!(distance_text(3_240.0, Units::Kilometres), "3.2 km");
        assert_eq!(distance_text(85_180.0, Units::Miles), "53 mi");
        assert_eq!(distance_text(100.0, Units::Miles), "330 ft");
        assert_eq!(distance_text(5_000.0, Units::Both), "3.1 mi · 5.0 km");
    }
}
