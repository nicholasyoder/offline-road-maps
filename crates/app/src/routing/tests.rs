//! The stops and the answers taken, on a worker with no thread.

use std::time::Duration;

use omp_core::route::{Leg, Trip};

use super::worker::{Answer, Request};
use super::*;

fn place(lat: f64, lon: f64) -> Place {
    Place::at(geo::lonlat_to_unit(lon, lat))
}

fn answer(r: &Request) -> Answer {
    let points: Vec<P> = r.stops.iter().map(|s| s.p).collect();
    let leg = Leg {
        weight: 10,
        time_s: 1.0,
        metres: 10.0,
        pieces: Vec::new(),
        points: points.clone(),
        uses: 0,
        broken: 0,
        settled: 0,
    };
    let trip = Trip {
        snaps: vec![None; r.stops.len()],
        legs: vec![Some(leg)],
        avoid: r.avoid,
    };
    let line = Line::new([points.as_slice()]);
    Answer {
        request: r.clone(),
        routed: Ok((trip, line)),
        trip: Duration::from_millis(2),
        line: Duration::ZERO,
    }
}

#[test]
fn stops_ask_and_the_latest_answer_is_shown() {
    let (worker, tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
    cam.size = [1000.0, 700.0];
    cam.set_lonlat(-83.0, 40.0, 16.0);

    // A start alone asks for nothing.
    r.apply(StopAction::From, place(40.0, -83.0));
    assert!(r.worker.take_request().is_none());
    assert!(r.menu_state().has_stops);

    // A destination asks for the route; a stop added goes between them.
    r.apply(StopAction::To, place(41.5, -81.7));
    let first = r.worker.take_request().unwrap();
    assert_eq!(first.stops.len(), 2);
    r.apply(StopAction::Add, place(40.8, -82.5));
    let second = r.worker.take_request().unwrap();
    let lats: Vec<f64> = second
        .stops
        .iter()
        .map(|s| geo::unit_to_lonlat(geo::to_unit(s.p))[1])
        .collect();
    assert!((lats[0] - 40.0).abs() < 1e-6 && (lats[1] - 40.8).abs() < 1e-6);
    assert!((lats[2] - 41.5).abs() < 1e-6);
    let kinds: Vec<StopAction> = r.stops().iter().map(|s| s.0).collect();
    assert_eq!(kinds, [StopAction::From, StopAction::Add, StopAction::To]);

    // The first request's answer is stale and dropped; the second's is shown, and the
    // view, at z16 on the start, flies out to fit it.
    tx.send(answer(&first)).unwrap();
    tx.send(answer(&second)).unwrap();
    let (c, z) = r.take(&cam).expect("fit the route");
    assert_eq!(r.dropped, 1);
    assert!(r.waiting.is_none());
    assert_eq!(r.route.as_ref().unwrap().trip.snaps.len(), 3);
    assert!((6.0..9.0).contains(&z), "{z}");
    let [lon, lat] = geo::unit_to_lonlat(c);
    assert!(
        (lat - 40.8).abs() < 0.2 && (lon + 82.35).abs() < 0.1,
        "{lat} {lon}"
    );

    // Already in view: no flight.
    cam.center = c;
    cam.zoom = z;
    r.reroute();
    let third = r.worker.take_request().unwrap();
    tx.send(answer(&third)).unwrap();
    assert!(r.take(&cam).is_none());
    assert!(r.route.is_some());

    // The line is drawn at the camera's band.
    let shown = r.route.as_ref().unwrap();
    let b = shown.line.band(band_for(cam.zoom));
    assert_eq!(b.segments.len(), 2);

    // Clearing drops the route and the stops.
    r.apply(StopAction::Clear, place(0.0, 0.0));
    assert!(r.route.is_none() && r.stops().is_empty());
    assert!(r.worker.take_request().is_none());
    assert!(!r.menu_state().has_stops);
}
