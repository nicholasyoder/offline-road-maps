//! The stops and the answers taken, on a worker with no thread.

use std::time::Duration;

use omp_core::route::{Kind, Leg, Modifier, Piece, Road, Snap, Step, Trip};

use super::worker::{self, Alternatives, Answer, Choices, Reply, Request};
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
                flags: 0,
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
        street_names: r
            .stops
            .iter()
            .map(|s| s.street.map(|i| format!("Street {i}")))
            .collect(),
        routed: Ok(routed),
        order: None,
        optimize: Duration::ZERO,
        trip: Duration::from_millis(2),
        directions: Duration::ZERO,
        line: Duration::ZERO,
    }
}

/// The alternatives to `a`'s route, one reply per leg: the quickest and one 4 min and 2 km
/// longer a little east of it, by "US 33", ranked best on the legs in `best`.
pub(super) fn alternatives(a: &Answer, best: &[usize]) -> Vec<Reply> {
    let routed = a.routed.as_ref().unwrap();
    let legs = routed
        .trip
        .legs
        .iter()
        .enumerate()
        .map(|(li, leg)| {
            let leg = leg.as_ref()?;
            let mut steps: Vec<Step> = routed
                .steps
                .iter()
                .filter(|s| s.leg == li)
                .cloned()
                .collect();
            let f = (steps[0].at_m, steps[0].at_s);
            for s in &mut steps {
                (s.at_m, s.at_s) = (s.at_m - f.0, s.at_s - f.1);
            }
            let quickest = worker::Route {
                leg: leg.clone(),
                turns: Default::default(),
                via: None,
                road: "I-71".into(),
                steps: steps.clone(),
                restricted: routed.restricted[li].clone(),
                line: Line::new([leg.points.as_slice()]),
                tag: Some(leg.points[1]),
            };
            let mut other = leg.clone();
            for p in &mut other.points {
                p[0] += 50_000;
            }
            for p in &mut other.pieces {
                p.dir += 100;
            }
            other.time_s += 240.0;
            other.metres += 2000.0;
            for s in &mut steps {
                s.metres += 500.0;
            }
            let alt = worker::Route {
                line: Line::new([other.points.as_slice()]),
                tag: Some(other.points[1]),
                via: Some(other.points[1]),
                leg: other,
                turns: Default::default(),
                road: "US 33".into(),
                steps,
                restricted: Vec::new(),
            };
            Some(Choices {
                routes: vec![quickest, alt],
                best: usize::from(best.contains(&li)),
            })
        })
        .collect();
    let legs: Vec<Option<Choices>> = legs;
    legs.into_iter()
        .enumerate()
        .map(|(leg, choices)| {
            Reply::Alternatives(Alternatives {
                id: a.request.id,
                leg,
                choices: Ok(choices),
                search: Duration::from_millis(5),
                directions: Duration::from_millis(7),
            })
        })
        .collect()
}

#[test]
fn alternatives_are_shown_and_picked() {
    let (worker, tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let mut cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
    cam.size = [1000.0, 700.0];
    r.apply(StopAction::From, place(40.0, -83.0));
    r.apply(StopAction::Add, place(40.5, -82.5));
    r.apply(StopAction::To, place(41.0, -82.0));
    let req = r.worker.take_request().unwrap();
    let a = answer(&req);
    let alts = alternatives(&a, &[1]);
    tx.send(a.into()).unwrap();
    r.take(&cam);
    cam.set_lonlat(-82.5, 40.5, 9.0);
    // Nothing is looked for until a leg is opened.
    assert!(!r.looking() && r.worker.take_alternatives().is_none());
    assert_eq!(r.expanded, [false, false]);
    let time = r.route.as_ref().unwrap().trip.time_s();

    // Opening both legs asks for both, in one job; each arrives on its own.
    r.expand(1, true);
    assert_eq!(r.worker.take_alternatives(), Some((req.id, vec![1])));
    r.expand(0, true);
    assert_eq!(r.worker.take_alternatives(), Some((req.id, vec![0, 1])));
    assert!(r.looking());
    let mut alts = alts.into_iter();
    tx.send(alts.next().unwrap()).unwrap();
    r.take(&cam);
    assert!(r.looking() && matches!(r.alts[0], LegAlts::Found(_)));
    tx.send(alts.next().unwrap()).unwrap();
    r.take(&cam);
    assert!(!r.looking());
    // The second leg's alternative ranks best with turn costs, but the quickest stays
    // shown (decision 23).
    assert_eq!(r.chosen, [0, 0]);
    assert_eq!(r.route.as_ref().unwrap().trip.time_s(), time);
    // Grey: both legs' alternatives.
    let grey = |r: &RouteUi| {
        r.overlays
            .iter()
            .find(|o| o.0 == Look::Alternative)
            .map_or(0, |o| o.1.points)
    };
    assert_eq!(grey(&r), 8);

    // Picking the second leg's alternative swaps in its line and steps.
    r.choose(1, 1);
    let shown = r.route.as_ref().unwrap();
    assert_eq!(shown.trip.time_s(), time + 240.0);
    assert_eq!(
        shown.trip.legs[1].as_ref().unwrap().points[0][0],
        req.stops[1].p[0] + 50_000
    );
    // Its steps are the alternative's, counted on from the first leg's.
    assert_eq!(shown.steps.len(), 6);
    assert_eq!(shown.steps[3].at_m, 3000.0);
    assert_eq!(shown.steps[5].at_m, 3000.0 + 1500.0 + 2500.0);
    assert!(shown.restricted[1].is_empty());
    assert_eq!(r.route.as_ref().unwrap().line.points, 8);
    // Grey: the first leg's alternative and the second's quickest.
    assert_eq!(grey(&r), 8);

    // Clicking near the first leg's grey line picks it; far from both, nothing.
    let alt = &r.not_shown().next().unwrap().2.leg.points;
    let mid = |a: P, b: P| [(a[0] / 2 + b[0] / 2), (a[1] / 2 + b[1] / 2)];
    let on = cam.world_to_screen(geo::to_unit(mid(alt[0], alt[1])));
    assert!(!r.click(&cam, [on[0] + 300.0, on[1]]));
    assert!(r.click(&cam, [on[0] + 3.0, on[1]]));
    assert_eq!(r.chosen, [1, 1]);
    assert_eq!(r.route.as_ref().unwrap().trip.time_s(), time + 480.0);
    // And the tag of the second leg's quickest, which says what it saves.
    let (li, k, q) = r.not_shown().nth(1).unwrap();
    assert_eq!((li, k), (1, 0));
    assert_eq!(r.tag_text(li, q), "−4 min");
    let tag = cam.world_to_screen(geo::to_unit(q.tag.unwrap()));
    assert!(r.click(&cam, [tag[0] + 20.0, tag[1] - 6.0]));
    assert_eq!(r.chosen, [1, 0]);

    // Closed, a leg's grey line goes and cannot be clicked; the route picked stays.
    r.expand(0, false);
    assert_eq!(grey(&r), 4);
    assert!(!r.click(&cam, [on[0] + 3.0, on[1]]));
    assert_eq!(r.chosen, [1, 0]);
    r.expand(1, false);
    assert_eq!(grey(&r), 0);
    assert_eq!(r.not_shown().count(), 0);
    // Opened again: shown at once, nothing asked.
    r.expand(0, true);
    assert_eq!(grey(&r), 4);
    assert!(r.worker.take_alternatives().is_none());

    // Answers for another route are dropped. A change of the stops lets go of the
    // routes; the leg left open is looked at again.
    let dropped = r.dropped;
    let other = Request {
        id: 99,
        ..req.clone()
    };
    for x in alternatives(&answer(&other), &[]) {
        tx.send(x).unwrap();
    }
    r.take(&cam);
    assert_eq!(r.dropped, dropped + 2);
    r.set_avoid(TOLL, true);
    // Opening a leg while the route is asked for waits for it.
    r.expand(1, true);
    let req = r.worker.take_request().unwrap();
    tx.send(answer(&req).into()).unwrap();
    r.take(&cam);
    assert_eq!(r.chosen, [0, 0]);
    assert_eq!(r.expanded, [true, true]);
    assert_eq!(r.worker.take_alternatives(), Some((req.id, vec![0, 1])));
    assert_eq!(grey(&r), 0);
    assert_eq!(delta_text(-20.0), "same time");
    assert_eq!(delta_text(3700.0), "+1 h 02 min");
}

#[test]
fn tags_go_where_routes_part() {
    let leg = |dirs: &[u32]| {
        let pieces: Vec<Piece> = dirs
            .iter()
            .enumerate()
            .map(|(i, &d)| Piece {
                dir: d,
                start: 0.0,
                end: 1.0,
                metres: 100.0,
                time_s: 10.0,
                points: i..i + 2,
                flags: 0,
            })
            .collect();
        Leg {
            weight: 0,
            time_s: 0.0,
            metres: 0.0,
            points: (0..=dirs.len() as u32).map(|i| [i * 10, 0]).collect(),
            pieces,
            uses: 0,
            broken: 0,
            settled: 0,
        }
    };
    let a = leg(&[2, 4, 6, 8, 10, 12, 14]);
    let b = leg(&[2, 30, 32, 34, 36, 38, 14]);
    // The run apart is pieces 1 to 5; its middle is piece 3's.
    assert_eq!(worker::tag_point(&b, &[&a]), Some([30, 0]));
    // Driven either way round, a segment is shared: 5 is segment 2 backwards.
    let c = leg(&[2, 5, 31, 33, 12, 14]);
    assert_eq!(worker::tag_point(&c, &[&a, &b]), None);
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
    tx.send(answer(&first).into()).unwrap();
    tx.send(answer(&second).into()).unwrap();
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
    tx.send(answer(&third).into()).unwrap();
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
    tx.send(answer(&req).into()).unwrap();
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

#[test]
fn saved_trips_open_unchanged_with_their_streets_and_routes() {
    let (worker, tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let cam = Camera::new([0.2, 0.3, 0.3, 0.4]);
    // A search result's stop, on its street; a point that stays the night; a point.
    let mut start = place(40.0, -83.0);
    start.street = Some(7);
    start.name = Some("1 Main St, Columbus".into());
    r.apply(StopAction::From, start);
    let mut night = place(40.5, -82.5);
    night.stay = Stay {
        s: 1800.0,
        overnight: true,
    };
    r.apply(StopAction::Add, night);
    r.apply(StopAction::To, place(41.0, -82.0));
    r.set_avoid(FERRY, true);
    r.ends.round_trip = true;
    let req = r.worker.take_request().unwrap();
    let a = answer(&req);
    let alts = alternatives(&a, &[]);
    tx.send(a.into()).unwrap();
    r.take(&cam);
    // The worker names the result's street, which the file keeps.
    assert_eq!(
        r.from.as_ref().unwrap().street_name.as_deref(),
        Some("Street 7")
    );
    // The second leg's alternative picked.
    r.expand(1, true);
    for x in alts {
        tx.send(x).unwrap();
    }
    r.take(&cam);
    r.choose(1, 1);
    let file = r.to_file("Test pack");
    let kinds: Vec<StopKind> = file.stops.iter().map(|s| s.kind).collect();
    use crate::trips::StopKind;
    assert_eq!(
        kinds,
        [
            StopKind::Stop,
            StopKind::Overnight,
            StopKind::Via,
            StopKind::Stop
        ]
    );
    assert_eq!(file.stops[0].street.as_deref(), Some("Street 7"));
    assert_eq!(file.stops[1].minutes, 30.0);
    assert!(file.avoid_ferries && file.optimize.round_trip);

    // Written and read back as it was.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.toml");
    crate::files::write(&path, &file).unwrap();
    let back = crate::trips::TripFile::read(&path).unwrap();
    assert_eq!(back, file);

    // Opened on another build: the street is found again by name, by the worker, and the
    // alternative passing the via point is picked again.
    let (worker, tx) = Worker::stub();
    let mut o = RouteUi::with_worker(worker);
    let named = crate::trips::Named {
        name: "t".into(),
        path: path.clone(),
        file: back.clone(),
    };
    o.open_file(&back, named);
    assert!(o.opened);
    let mut req = o.worker.take_request().unwrap();
    assert_eq!(req.names, [Some("Street 7".into()), None, None]);
    assert_eq!(
        req.stops.iter().map(|s| s.p).collect::<Vec<_>>(),
        r.routed_stops
    );
    assert!(req.stops[0].street.is_none());
    req.stops[0].street = Some(9);
    let a = answer(&req);
    let alts = alternatives(&a, &[]);
    tx.send(a.into()).unwrap();
    o.take(&cam);
    let from = o.from.as_ref().unwrap();
    assert_eq!(
        (from.street, from.street_name.as_deref()),
        (Some(9), Some("Street 7"))
    );
    // The leg with a route saved is looked at at once, without opening it.
    assert_eq!(o.worker.take_alternatives(), Some((req.id, vec![1])));
    assert!(!o.unsaved(), "a trip just opened has no changes");
    for x in alts {
        tx.send(x).unwrap();
    }
    o.take(&cam);
    assert_eq!(o.chosen, [0, 1]);
    assert_eq!(o.expanded, [false, false]);
    assert_eq!(o.to_file("Test pack"), file);
    assert!(!o.unsaved());
    o.set_stay(1, Stay::default());
    assert!(o.unsaved());

    // On a build where no alternative passes it, the quickest is shown, and said so.
    let (worker, tx) = Worker::stub();
    let mut o = RouteUi::with_worker(worker);
    let named = crate::trips::Named {
        name: "t".into(),
        path,
        file: back.clone(),
    };
    o.open_file(&back, named);
    let req = o.worker.take_request().unwrap();
    let a = answer(&req);
    let mut alts = alternatives(&a, &[]);
    if let Reply::Alternatives(x) = &mut alts[1]
        && let Ok(Some(c)) = &mut x.choices
    {
        for p in &mut c.routes[1].leg.points {
            p[1] += 900_000;
        }
    }
    tx.send(a.into()).unwrap();
    o.take(&cam);
    for x in alts {
        tx.send(x).unwrap();
    }
    o.take(&cam);
    assert_eq!(o.chosen, [0, 0]);
    let n = o.notices(Units::Kilometres);
    assert!(
        n.iter()
            .any(|t| t.contains("stop 2 to the destination was not found")),
        "{n:?}"
    );
}

#[test]
fn a_map_stop_is_named_by_the_address_found_near_it() {
    let (worker, _tx) = Worker::stub();
    let mut r = RouteUi::with_worker(worker);
    let (at, other) = (place(40.0, -83.0), place(41.0, -82.0));
    r.apply(StopAction::From, at.clone());
    r.apply(StopAction::To, other.clone());
    let a = omp_core::here::HereAddress {
        label: "12 Main Street".into(),
        p: at.p,
        dist_m: 20.0,
        approx: false,
        street: Some(7),
        postcode: None,
        units: Vec::new(),
    };
    r.name_stops_at(at.p, &a);
    let from = r.from.as_ref().unwrap();
    assert_eq!(from.name.as_deref(), Some("12 Main Street"));
    assert_eq!(from.street, Some(7));
    // Another stop is left as it was, and a named one is not renamed.
    assert_eq!(r.to, Some(other));
    r.name_stops_at(
        at.p,
        &omp_core::here::HereAddress {
            label: "9 Elm".into(),
            ..a
        },
    );
    assert_eq!(
        r.from.as_ref().unwrap().name.as_deref(),
        Some("12 Main Street")
    );
}
