//! Criterion 4's trips (phase 4 step 4.5): straight legs through boxes named after the real
//! zones, timed with the real rules.

use jiff::civil::date;

use super::*;
use crate::geo::project;
use crate::route::{Modifier, Piece, Road};
use crate::tz::{Zones, encode};

/// A box of lat/lon as a ring.
fn boxed(s: f64, w: f64, n: f64, e: f64) -> Vec<P> {
    vec![project(n, w), project(n, e), project(s, e), project(s, w)]
}

fn zones(z: &[(&str, Vec<Vec<P>>)]) -> Zones {
    let owned: Vec<(String, Vec<Vec<P>>)> =
        z.iter().map(|(n, r)| (n.to_string(), r.clone())).collect();
    Zones::decode(&encode(&owned, 3).unwrap()).unwrap()
}

/// A straight leg of `hours` from `a` to `b` (lat, lon), as one piece of 200 points.
fn leg(a: (f64, f64), b: (f64, f64), hours: f64) -> Leg {
    let points: Vec<P> = (0..=200)
        .map(|i| {
            let f = f64::from(i) / 200.0;
            project(a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
        })
        .collect();
    let time_s = hours * 3600.0;
    Leg {
        weight: (time_s * 10.0) as u32,
        time_s,
        metres: 0.0,
        pieces: vec![Piece {
            dir: 0,
            start: 0.0,
            end: 1.0,
            metres: 0.0,
            time_s,
            points: 0..points.len(),
            flags: 0,
        }],
        points,
        uses: 0,
        broken: 0,
        settled: 0,
    }
}

/// The trip through `stops` with legs of `hours`, and its stops' points.
fn trip(stops: &[(f64, f64)], hours: &[f64]) -> (Trip, Vec<P>) {
    let legs = stops
        .windows(2)
        .zip(hours)
        .map(|(w, &h)| Some(leg(w[0], w[1], h)))
        .collect();
    let trip = Trip {
        snaps: vec![None; stops.len()],
        legs,
        avoid: 0,
    };
    (
        trip,
        stops.iter().map(|&(la, lo)| project(la, lo)).collect(),
    )
}

/// `y-m-d h:m` in `zone`.
fn at(zone: &str, y: i16, m: i8, d: i8, h: i8, min: i8) -> Timestamp {
    let tz = jiff::tz::TimeZoneDatabase::bundled().get(zone).unwrap();
    tz.to_timestamp(date(y, m, d).at(h, min, 0, 0)).unwrap()
}

/// "2026-07-15 13:00 MST".
fn show(z: &Zoned) -> String {
    z.strftime("%Y-%m-%d %H:%M %Z").to_string()
}

fn settings(when: When, n: usize) -> Settings {
    Settings {
        when,
        stays: vec![Stay::default(); n],
        day: None,
        breaks: None,
    }
}

/// A step every 10 of each leg's 200 points, as directions would give.
fn steps(trip: &Trip) -> Vec<Step> {
    let mut out = Vec::new();
    let mut base = 0.0;
    for (li, l) in trip.legs.iter().flatten().enumerate() {
        for k in (0..200).step_by(10) {
            let f = k as f64 / 200.0;
            out.push(Step {
                kind: crate::route::Kind::Continue,
                modifier: Modifier::Straight,
                angle: 0.0,
                road: Road::default(),
                stay: false,
                exit: String::new(),
                toward: String::new(),
                roundabout_exit: 0,
                side: None,
                heading: "",
                stop: 0,
                leg: li,
                pieces: 0..1,
                points: k..k + 11,
                point: l.points[k],
                metres: 0.0,
                time_s: l.time_s / 20.0,
                at_m: 0.0,
                at_s: base + f * l.time_s,
            });
        }
        base += l.time_s;
    }
    out
}

#[test]
fn across_arizona_in_summer() {
    let z = zones(&[
        (
            "America/Los_Angeles",
            vec![boxed(30.0, -125.0, 40.0, -114.5)],
        ),
        ("America/Phoenix", vec![boxed(30.0, -114.5, 40.0, -109.0)]),
        ("America/Denver", vec![boxed(30.0, -109.0, 40.0, -100.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(
        &[(34.0, -118.0), (33.5, -112.0), (35.0, -106.6)],
        &[5.0, 6.0],
    );
    let mut st = settings(
        When::Depart(at("America/Los_Angeles", 2026, 7, 15, 8, 0)),
        3,
    );
    st.stays[1].s = 3600.0;
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(s.departure().unwrap()), "2026-07-15 08:00 PDT");
    // Phoenix keeps MST all year: the same clock as Los Angeles in summer.
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-07-15 13:00 MST");
    assert_eq!(show(s.depart[1].as_ref().unwrap()), "2026-07-15 14:00 MST");
    assert_eq!(show(s.arrival().unwrap()), "2026-07-15 21:00 MDT");
    let dep = s.departure().unwrap().time_zone().clone();
    let a1 = s.arrive[1].as_ref().unwrap();
    assert_eq!(difference(a1.timestamp(), a1.time_zone(), &dep), 0);
    let a2 = s.arrival().unwrap();
    assert_eq!(difference(a2.timestamp(), a2.time_zone(), &dep), 3600);

    let c = crossings(&trip, &steps(&trip), &s, &local);
    let seen: Vec<(String, i32)> = c.iter().map(|c| (c.abbr.clone(), c.change_s)).collect();
    assert_eq!(seen, [("MST".to_string(), 0), ("MDT".to_string(), 3600)]);
}

#[test]
fn indianas_split_counties() {
    // Central in the northwest and southwest corners, Eastern between.
    let sw = boxed(37.0, -87.5, 38.3, -86.9);
    let z = zones(&[
        (
            "America/Chicago",
            vec![boxed(37.0, -90.0, 43.0, -87.5), sw.clone()],
        ),
        (
            "America/Indiana/Indianapolis",
            vec![boxed(37.0, -87.5, 43.0, -84.8), sw],
        ),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(
        &[(41.88, -87.63), (39.77, -86.16), (37.97, -87.57)],
        &[3.0, 3.0],
    );
    let st = settings(When::Depart(at("America/Chicago", 2026, 10, 10, 9, 0)), 3);
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-10-10 13:00 EDT");
    assert_eq!(show(s.arrival().unwrap()), "2026-10-10 15:00 CDT");
    let c = crossings(&trip, &steps(&trip), &s, &local);
    let seen: Vec<(String, i32)> = c.iter().map(|c| (c.abbr.clone(), c.change_s)).collect();
    assert_eq!(
        seen,
        [("EDT".to_string(), 3600), ("CDT".to_string(), -3600)]
    );
}

#[test]
fn saskatchewan_keeps_central_standard_time() {
    let z = zones(&[
        ("America/Winnipeg", vec![boxed(45.0, -102.0, 55.0, -95.0)]),
        ("America/Regina", vec![boxed(45.0, -110.0, 55.0, -102.0)]),
        ("America/Edmonton", vec![boxed(45.0, -120.0, 55.0, -110.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(
        &[(49.9, -97.1), (50.45, -104.6), (51.05, -114.07)],
        &[6.0, 7.0],
    );
    let summer = settings(When::Depart(at("America/Winnipeg", 2026, 7, 1, 7, 0)), 3);
    let s = schedule(&trip, &stops, &summer, &local).unwrap();
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-07-01 12:00 CST");
    assert_eq!(show(s.arrival().unwrap()), "2026-07-01 19:00 MDT");
    let c = crossings(&trip, &steps(&trip), &s, &local);
    let seen: Vec<(String, i32)> = c.iter().map(|c| (c.abbr.clone(), c.change_s)).collect();
    assert_eq!(seen, [("CST".to_string(), -3600), ("MDT".to_string(), 0)]);

    // In winter Manitoba is on CST too: no crossing there at all.
    let winter = settings(When::Depart(at("America/Winnipeg", 2026, 1, 15, 7, 0)), 3);
    let s = schedule(&trip, &stops, &winter, &local).unwrap();
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-01-15 13:00 CST");
    assert_eq!(show(s.arrival().unwrap()), "2026-01-15 19:00 MST");
    let c = crossings(&trip, &steps(&trip), &s, &local);
    let seen: Vec<(String, i32)> = c.iter().map(|c| (c.abbr.clone(), c.change_s)).collect();
    assert_eq!(seen, [("MST".to_string(), -3600)]);
}

#[test]
fn newfoundlands_half_hour() {
    let z = zones(&[
        ("America/Halifax", vec![boxed(42.0, -67.0, 50.0, -59.5)]),
        ("America/St_Johns", vec![boxed(42.0, -59.5, 50.0, -52.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(44.65, -63.57), (47.56, -52.71)], &[12.0]);
    let st = settings(When::Depart(at("America/Halifax", 2026, 8, 1, 6, 0)), 2);
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    let a = s.arrival().unwrap();
    assert_eq!(show(a), "2026-08-01 18:30 NDT");
    let dep = s.departure().unwrap().time_zone().clone();
    assert_eq!(difference(a.timestamp(), a.time_zone(), &dep), 1800);
    let c = crossings(&trip, &steps(&trip), &s, &local);
    assert_eq!(c.len(), 1);
    assert_eq!((c[0].abbr.as_str(), c[0].change_s), ("NDT", 1800));
    // Where the line crosses 59.5° W, 37% of the way: 10:30 ADT, which is 11:00 NDT.
    let cross = s.clock(c[0].at_s, false).to_zoned(c[0].tz.clone());
    let want = at("America/St_Johns", 2026, 8, 1, 11, 0).as_second();
    let off = cross.timestamp().as_second() - want;
    assert!(off.abs() <= 180, "{}", show(&cross));
}

#[test]
fn a_dst_change_during_the_night() {
    let z = zones(&[("America/Chicago", vec![boxed(30.0, -100.0, 45.0, -85.0)])]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(41.88, -87.63), (35.0, -97.5)], &[15.0]);
    let mut st = settings(When::Depart(at("America/Chicago", 2026, 10, 31, 8, 0)), 2);
    st.day = Some(Day::default());
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(s.events.len(), 1);
    let night = &s.events[0];
    assert_eq!(night.kind, Kind::Night);
    assert_eq!(night.day, 2);
    assert_eq!(show(&night.from), "2026-10-31 18:00 CDT");
    // Clocks go back at 2:00: the morning's 8:00 is 15 hours on, not 14.
    assert_eq!(show(&night.until), "2026-11-01 08:00 CST");
    assert_eq!(
        night.until.timestamp().as_second() - night.from.timestamp().as_second(),
        15 * 3600
    );
    assert_eq!(show(s.arrival().unwrap()), "2026-11-01 13:00 CST");
    assert_eq!(s.days(), 2);
    // A step's clock: two hours into each day.
    let tz = s.departure().unwrap().time_zone().clone();
    let clock = |at_s: f64| show(&s.clock(at_s, false).to_zoned(tz.clone()));
    assert_eq!(clock(2.0 * 3600.0), "2026-10-31 10:00 CDT");
    assert_eq!(clock(12.0 * 3600.0), "2026-11-01 10:00 CST");
    // At the night itself, the morning's start; arriving there, the evening's.
    assert_eq!(clock(10.0 * 3600.0), "2026-11-01 08:00 CST");
    assert_eq!(
        show(&s.clock(10.0 * 3600.0, true).to_zoned(tz)),
        "2026-10-31 18:00 CDT"
    );
}

#[test]
fn a_window_shorter_than_the_first_leg() {
    let local = Local {
        zones: None,
        fallback: jiff::tz::TimeZoneDatabase::bundled()
            .get("America/Chicago")
            .unwrap(),
    };
    let (trip, stops) = trip(
        &[(41.88, -87.63), (35.0, -97.5), (32.8, -96.8)],
        &[12.0, 3.0],
    );
    let mut st = settings(When::Depart(at("America/Chicago", 2026, 9, 1, 8, 0)), 3);
    st.day = Some(Day::default());
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    // The first stop falls on day 2.
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-09-02 10:00 CDT");
    assert_eq!(show(s.arrival().unwrap()), "2026-09-02 13:00 CDT");
    assert_eq!(s.events[0].leg, 0);
    assert_eq!(s.events[0].at_s, 10.0 * 3600.0);

    // At most 8 hours a day, with no end: 8:00–16:00, then the next morning.
    st.day = Some(Day {
        end: None,
        max_drive_s: Some(8.0 * 3600.0),
        ..Day::default()
    });
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(&s.events[0].from), "2026-09-01 16:00 CDT");
    assert_eq!(show(s.arrival().unwrap()), "2026-09-02 15:00 CDT");

    // Leaving before the window waits for it.
    st.day = Some(Day::default());
    st.when = When::Depart(at("America/Chicago", 2026, 9, 1, 5, 30));
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert!(s.waited);
    assert_eq!(show(s.departure().unwrap()), "2026-09-01 08:00 CDT");
}

#[test]
fn arrive_by_over_several_days() {
    let local = Local {
        zones: None,
        fallback: jiff::tz::TimeZoneDatabase::bundled()
            .get("America/Chicago")
            .unwrap(),
    };
    let (trip, stops) = trip(
        &[(41.88, -87.63), (35.0, -97.5), (32.8, -96.8)],
        &[12.0, 13.0],
    );
    let mut st = settings(When::ArriveBy(at("America/Chicago", 2026, 9, 16, 12, 0)), 3);
    st.day = Some(Day::default());
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    // 4 h on the last morning, two full days of 10, and the first day's last hour.
    assert_eq!(show(s.departure().unwrap()), "2026-09-13 17:00 CDT");
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-09-15 09:00 CDT");
    assert_eq!(show(s.arrival().unwrap()), "2026-09-16 12:00 CDT");
    assert_eq!(s.days(), 4);

    // Overnight at the stop: it leaves the next morning.
    st.stays[1].overnight = true;
    st.when = When::Depart(at("America/Chicago", 2026, 9, 1, 8, 0));
    st.day = None;
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(s.arrive[1].as_ref().unwrap()), "2026-09-01 20:00 CDT");
    assert_eq!(show(s.depart[1].as_ref().unwrap()), "2026-09-02 08:00 CDT");
    assert_eq!(s.events[0].stop, Some(1));
}

#[test]
fn breaks_every_two_hours() {
    let local = Local {
        zones: None,
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(41.88, -87.63), (35.0, -97.5)], &[5.0]);
    let mut st = settings(When::Depart(at("UTC", 2026, 9, 1, 8, 0)), 2);
    st.breaks = Some(Breaks {
        every_s: 7200.0,
        for_s: 900.0,
    });
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    let breaks: Vec<String> = s.events.iter().map(|e| show(&e.from)).collect();
    assert_eq!(breaks, ["2026-09-01 10:00 UTC", "2026-09-01 12:15 UTC"]);
    assert_eq!(show(s.arrival().unwrap()), "2026-09-01 13:30 UTC");
    // A break every 0 hours (a hand-edited file) is none, rather than a trip that never
    // moves on.
    for every_s in [0.0, -1.0, f64::NAN] {
        st.breaks = Some(Breaks {
            every_s,
            for_s: 900.0,
        });
        let s = schedule(&trip, &stops, &st, &local).unwrap();
        assert!(s.events.is_empty(), "{every_s}");
    }
}

#[test]
fn slivers_between_zones_are_dropped() {
    // A sliver of Mountain Time 0.01° wide (seconds of driving) just before the border.
    let z = zones(&[
        ("America/Boise", vec![boxed(30.0, -87.52, 45.0, -87.51)]),
        ("America/Chicago", vec![boxed(30.0, -90.0, 45.0, -87.5)]),
        ("America/New_York", vec![boxed(30.0, -87.5, 45.0, -80.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(40.0, -89.0), (40.0, -86.0)], &[3.0]);
    let st = settings(When::Depart(at("America/Chicago", 2026, 9, 1, 8, 0)), 2);
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    let c = crossings(&trip, &steps(&trip), &s, &local);
    let seen: Vec<(String, i32)> = c.iter().map(|c| (c.abbr.clone(), c.change_s)).collect();
    assert_eq!(seen, [("EDT".to_string(), 3600)]);
}

#[test]
fn the_day_ends_by_the_clock_where_the_driver_is() {
    // Into Eastern Time an hour before the leg ends: 18:00 there is 17:00 at the start.
    let z = zones(&[
        ("America/Chicago", vec![boxed(30.0, -90.0, 45.0, -87.0)]),
        ("America/New_York", vec![boxed(30.0, -87.0, 45.0, -80.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(40.0, -89.0), (40.0, -86.0)], &[3.0]);
    let mut st = settings(When::Depart(at("America/Chicago", 2026, 9, 1, 15, 0)), 2);
    st.day = Some(Day::default());
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(&s.events[0].from), "2026-09-01 18:00 EDT");
    assert_eq!(show(&s.events[0].until), "2026-09-02 08:00 EDT");
    assert_eq!(show(s.arrival().unwrap()), "2026-09-02 09:00 EDT");
}

#[test]
fn times_as_typed() {
    let t = |s: &str| parse_time(s).map(|t| t.to_string());
    assert_eq!(t("08:00").as_deref(), Some("08:00:00"));
    assert_eq!(t(" 8:30 ").as_deref(), Some("08:30:00"));
    assert_eq!(t("18").as_deref(), Some("18:00:00"));
    for bad in ["", "24:00", "8:60", "8:", "x", "8:005"] {
        assert_eq!(t(bad), None, "{bad:?}");
    }
}

#[test]
fn a_day_that_starts_in_one_zone_ends_by_the_next() {
    // 8:00 CDT is 7:00 MDT, before the window there; the day still ends at 18:00 MDT.
    let z = zones(&[
        ("America/Chicago", vec![boxed(30.0, -100.0, 45.0, -95.0)]),
        ("America/Denver", vec![boxed(30.0, -110.0, 45.0, -100.0)]),
    ]);
    let local = Local {
        zones: Some(&z),
        fallback: TimeZone::UTC,
    };
    let (trip, stops) = trip(&[(40.0, -99.0), (40.0, -109.0)], &[12.0]);
    let mut st = settings(When::Depart(at("America/Chicago", 2026, 7, 13, 8, 0)), 2);
    st.day = Some(Day::default());
    let s = schedule(&trip, &stops, &st, &local).unwrap();
    assert_eq!(show(&s.events[0].from), "2026-07-13 18:00 MDT");
}
