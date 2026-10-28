//! The stops and the answers taken, on a worker with no thread.

use std::time::Duration;

use omp_core::route::{Kind, Leg, Modifier, Piece, Road, Snap, Step, Trip};

use super::worker::{Answer, Request};
use super::*;
use crate::scale::Units;

fn place(lat: f64, lon: f64) -> Place {
    Place::at(geo::lonlat_to_unit(lon, lat))
}

fn step(kind: Kind, leg: usize, points: std::ops::Range<usize>, metres: f64) -> Step {
    Step {
        kind,
        modifier: Modifier::Straight,
        angle: 0.0,
        road: Road::default(),
        stay: false,
        exit: String::new(),
        toward: String::new(),
        roundabout_exit: 0,
        side: None,
        heading: "north",
        stop: 0,
        leg,
        pieces: 0..0,
        points,
        point: [0, 0],
        metres,
        time_s: metres / 10.0,
        at_m: 0.0,
        at_s: 0.0,
    }
}

/// A route straight through the stops, one leg each: three pieces per leg, the last on a
/// restricted road; each stop snapped 600 m away, past [`FAR_SNAP_M`].
pub(super) fn answer(r: &Request) -> Answer {
    let n = r.stops.len();
    let mut legs = Vec::new();
    let mut steps = Vec::new();
    for (i, w) in r.stops.windows(2).enumerate() {
        let (a, b) = (w[0].p, w[1].p);
        let lerp = |t: f64| {
            let f = |k: usize| (f64::from(a[k]) + (f64::from(b[k]) - f64::from(a[k])) * t) as u32;
            [f(0), f(1)]
        };
        let points: Vec<P> = (0..=3).map(|k| lerp(f64::from(k) / 3.0)).collect();
        let pieces = (0..3)
            .map(|k| Piece {
                dir: k as u32 * 2,
                start: 0.0,
                end: 1.0,
                metres: 1000.0,
                time_s: 60.0,
                points: k..k + 2,
            })
            .collect();
        legs.push(Some(Leg {
            weight: 1800,
            time_s: 180.0,
            metres: 3000.0,
            pieces,
            points,
            uses: 0,
            broken: r.avoid & TOLL,
            settled: 0,
        }));
        steps.push(step(Kind::Depart, i, 0..2, 1000.0));
        steps.push(step(Kind::Turn, i, 1..4, 2000.0));
        let end = if i + 2 == n { Kind::Arrive } else { Kind::Stop };
        steps.push(step(end, i, 3..4, 0.0));
    }
    let snaps = r
        .stops
        .iter()
        .map(|s| {
            Some(Snap {
                segment: 0,
                along: 0.0,
                point: [s.p[0] + 100_000, s.p[1]],
                dist_m: 600.0,
                on_street: false,
                small: false,
            })
        })
        .collect();
    let trip = Trip {
        snaps,
        legs,
        avoid: r.avoid,
    };
    let line = Line::of_trip(&trip);
    let routed = Routed {
        restricted: vec![vec![2..3]; n - 1],
        roads: vec!["Main St".into(); n],
        steps,
        trip,
        line,
    };
    Answer {
        request: r.clone(),
        routed: Ok(routed),
        trip: Duration::from_millis(2),
        directions: Duration::ZERO,
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

    // The line is drawn at the camera's band: collinear points go.
    let shown = r.route.as_ref().unwrap();
    let b = shown.line.band(band_for(cam.zoom));
    assert_eq!(b.segments.len(), 2);
    // Over it, the restricted pieces and the stops' connectors.
    let looks: Vec<Look> = r.overlays.iter().map(|o| o.0).collect();
    assert_eq!(looks, [Look::Connector, Look::Restricted]);
    assert_eq!(r.overlays[0].1.points, 6);
    assert_eq!(r.overlays[1].1.points, 4);

    // Clearing drops the route and the stops.
    r.apply(StopAction::Clear, place(0.0, 0.0));
    assert!(r.route.is_none() && r.stops().is_empty());
    assert!(r.worker.take_request().is_none());
    assert!(!r.menu_state().has_stops);
}

#[test]
fn stops_are_moved_removed_and_reversed() {
    let (worker, _tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let names = |r: &RouteUi| -> Vec<(StopAction, String)> {
        r.stops()
            .iter()
            .map(|(k, p)| (*k, p.name.clone().unwrap()))
            .collect()
    };
    let named = |n: &str, lat: f64| Place {
        name: Some(n.into()),
        ..place(lat, -83.0)
    };
    use StopAction::{Add, From, To};
    r.apply(From, named("a", 40.0));
    r.apply(Add, named("b", 40.1));
    r.apply(Add, named("c", 40.2));
    r.apply(To, named("d", 40.3));
    assert!(r.opened);
    let order = |r: &RouteUi| names(r).into_iter().map(|n| n.1).collect::<String>();
    // The last goes first: it is the start now, and the old start a stop between.
    r.move_stop(3, 0);
    assert_eq!(order(&r), "dabc");
    assert_eq!(names(&r)[0].0, From);
    assert_eq!(names(&r)[3], (To, "c".into()));
    let req = r.worker.take_request().unwrap();
    assert_eq!(req.stops.len(), 4);
    r.reverse();
    assert_eq!(order(&r), "cbad");
    // Removing the start: the next stop starts.
    r.remove_stop(0);
    assert_eq!(order(&r), "bad");
    assert_eq!(names(&r)[0].0, From);
    // Removing the destination with no stops between leaves none.
    r.remove_stop(1);
    r.remove_stop(1);
    assert_eq!(names(&r), [(From, "b".into())]);
    assert!(r.worker.take_request().is_none());
    // Reversing a start alone makes it the destination.
    r.reverse();
    assert_eq!(names(&r), [(To, "b".into())]);
    // Avoid options ask again only when they change.
    r.apply(From, named("e", 41.0));
    r.worker.take_request();
    r.set_avoid(TOLL, true);
    assert_eq!(r.worker.take_request().unwrap().avoid, TOLL);
    r.set_avoid(TOLL, true);
    assert!(r.worker.take_request().is_none());
    r.set_avoid(TOLL, false);
    assert_eq!(r.worker.take_request().unwrap().avoid, 0);
}

#[test]
fn notices_and_steps() {
    let (worker, tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
    cam.size = [1000.0, 700.0];
    r.apply(StopAction::From, place(40.0, -83.0));
    r.apply(StopAction::Add, place(40.5, -82.5));
    r.apply(StopAction::To, place(41.0, -82.0));
    r.set_avoid(TOLL, true);
    let req = r.worker.take_request().unwrap();
    tx.send(answer(&req)).unwrap();
    r.take(&cam);
    let n = r.notices(Units::Miles);
    assert_eq!(
        n,
        [
            "The start is 0.4 mi from the nearest road.",
            "Stop 2 is 0.4 mi from the nearest road.",
            "The destination is 0.4 mi from the nearest road.",
            "This route uses a toll road: no route avoids them.",
            "0.6 mi before stop 2 is on a road with restricted access (private or destination only).",
            "The last 0.6 mi is on a road with restricted access (private or destination only).",
        ]
    );
    // Picking a step draws its stretch, and the view fits it.
    r.pick(Some(4));
    let picked = r.overlays.iter().find(|o| o.0 == Look::Picked).unwrap();
    assert_eq!(picked.1.points, 3);
    let (c, z) = r.step_view(4, cam.size).unwrap();
    let [lon, lat] = geo::unit_to_lonlat(c);
    assert!(
        (lat - 40.833).abs() < 0.01 && (lon + 82.167).abs() < 0.01,
        "{lat} {lon}"
    );
    assert!(z > 8.0 && z < 12.0, "{z}");
    r.pick(None);
    assert!(r.overlays.iter().all(|o| o.0 != Look::Picked));
    assert_eq!(duration_text(20.0), "under a minute");
    assert_eq!(duration_text(3.0 * 3600.0 + 290.0), "3 h 05 min");
}
