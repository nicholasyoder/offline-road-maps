use super::*;
use crate::admin::read::{AdminData, Area, ReadStats};
use crate::geom::project;
use crate::street::grid;

fn area(id: i64, level: u8, name: &str, sw: (f64, f64), ne: (f64, f64)) -> Area {
    let (sw, ne) = (project(sw.0, sw.1), project(ne.0, ne.1));
    Area {
        osm_id: id,
        level,
        name: name.into(),
        alt_names: Vec::new(),
        iso: (level == 4).then(|| "US-OH".into()),
        population: None,
        border_type: None,
        place: None,
        label: None,
        rings: vec![vec![
            [sw[0], ne[1]],
            [ne[0], ne[1]],
            [ne[0], sw[1]],
            [sw[0], sw[1]],
        ]],
        broken: false,
    }
}

fn admin() -> Admin {
    Admin::build(AdminData {
        areas: vec![
            area(1, 4, "Ohio", (38.5, -85.0), (42.0, -80.5)),
            area(2, 8, "Westville", (39.9, -83.2), (40.1, -83.0)),
            area(3, 8, "Eastville", (39.9, -83.0), (40.1, -82.8)),
        ],
        places: Vec::new(),
        stats: ReadStats::default(),
    })
}

fn way(id: i64, name: &str, nodes: &[(i64, f64, f64)]) -> StreetWay {
    StreetWay {
        id,
        names: vec![name.into()],
        nnames: 1,
        class: osm::highway_class("residential"),
        refs: nodes.iter().map(|n| n.0).collect(),
        coords: nodes.iter().map(|n| project(n.1, n.2)).collect(),
    }
}

/// Main Street runs east from Westville into Eastville (way 0, cut at the boundary) and
/// on as "Main St" (way 1); County Road 5 is a 60 km rural way (way 2); Elm St is a T of
/// two ways (3, 4); Oak Avenue crosses Main Street without joining it (way 5).
fn ways() -> Vec<StreetWay> {
    let main: Vec<(i64, f64, f64)> = (0..=20)
        .map(|i| (1 + i, 40.0, -83.155 + 0.01 * i as f64))
        .collect();
    let main2: Vec<(i64, f64, f64)> = (0..=5)
        .map(|i| (21 + i, 40.0, -82.955 + 0.01 * i as f64))
        .collect();
    let county: Vec<(i64, f64, f64)> = (0..=70)
        .map(|i| (1000 + i, 39.0, -84.5 + 0.01 * i as f64))
        .collect();
    let elm: Vec<(i64, f64, f64)> = (0..=4)
        .map(|i| (100 + i, 40.005, -83.06 + 0.01 * i as f64))
        .collect();
    let spur = [
        (102, 40.005, -83.04),
        (110, 40.015, -83.04),
        (111, 40.025, -83.04),
    ];
    let oak = [
        (200, 39.99, -83.105),
        (5, 40.0, -83.115),
        (201, 40.01, -83.125),
    ];
    vec![
        way(10, "Main Street", &main),
        way(11, "Main St", &main2),
        way(12, "County Road 5", &county),
        way(13, "Elm St", &elm),
        way(14, "Elm Street", &spur),
        way(15, "Oak Avenue", &oak),
    ]
}

fn named<'a>(t: &'a Table, key: &str) -> Vec<&'a Street> {
    t.streets
        .iter()
        .filter(|s| street_key(&s.name) == street_key(key))
        .collect()
}

#[test]
fn streets_split_by_town_and_length() {
    let (admin, ways) = (admin(), ways());
    let t = Table::build(&ways, &admin, &[], &[]);
    // Main Street: the Westville piece, and the Eastville piece with Main St.
    let main = named(&t, "Main Street");
    assert_eq!(main.len(), 2);
    let mut towns: Vec<&str> = main.iter().map(|s| s.town.as_str()).collect();
    towns.sort_unstable();
    assert_eq!(towns, ["Eastville", "Westville"]);
    assert!(main.iter().all(|s| s.town_source == Source::Admin));
    assert_eq!(t.stats.cuts_area, 1);

    // County Road 5: 60 km in pieces of 5 km, in streets of 20–25 km.
    let county = named(&t, "County Road 5");
    assert_eq!(county.len(), 3);
    for s in &county {
        let len = s.along.length_m();
        assert!((15_000.0..=25_500.0).contains(&len), "{len}");
        assert_eq!(s.parts.len(), 1);
    }
    assert!(t.stats.cuts_length >= 11);

    // Elm St: one street of two parts, the spur its own part.
    let elm = named(&t, "Elm St");
    assert_eq!(elm.len(), 1);
    assert_eq!(elm[0].parts.len(), 2);
    assert_eq!(named(&t, "Oak Avenue").len(), 1);
    // Straight lines keep only their ends.
    assert_eq!(elm[0].parts[0].len(), 2);
    // The spur starts on a fresh 5 m step, so rounded offsets near either end of the gap
    // stay on their own part.
    let a = &elm[0].along;
    let end0 = *a.cum[0].last().unwrap();
    let start1 = a.cum[1][0];
    assert_eq!(start1 % STEP_M, 0.0);
    assert!(start1 >= (end0 / STEP_M).ceil() * STEP_M + STEP_M);
    let round = |m: f64| (m / STEP_M).round() * STEP_M;
    let at = |m: f64| point_at(&elm[0].parts, a, m);
    let near_end = dist(at(round(end0 - 0.1)), at(end0)) * a.mpu;
    assert!(near_end <= STEP_M / 2.0 + 0.1, "{near_end}");
    assert_eq!(at(round(start1 + 0.1)), elm[0].parts[1][0]);
}

#[test]
fn postal_town_joins_across_the_boundary() {
    let (admin, ways) = (admin(), ways());
    let cities = vec!["SPRINGFIELD".to_owned(), String::new()];
    let votes = [
        (0, ways[0].coords[3], 0),
        (0, ways[0].coords[18], 0),
        (1, ways[1].coords[2], 0),
        (1, ways[1].coords[3], 1),
    ];
    let t = Table::build(&ways, &admin, &votes, &cities);
    let main = named(&t, "Main Street");
    assert_eq!(main.len(), 1);
    assert_eq!(main[0].town, "Springfield");
    assert_eq!(main[0].town_source, Source::Addresses);
    // Elm St has no addresses; its neighbours in Westville give Springfield.
    let elm = named(&t, "Elm St");
    assert_eq!(elm[0].town, "Springfield");
    assert_eq!(elm[0].town_source, Source::Neighbours);
    assert_eq!(t.stats.by_postal_town, 1);
}

#[test]
fn every_way_point_locates_on_its_street() {
    let (admin, ways) = (admin(), ways());
    let t = Table::build(&ways, &admin, &[], &[]);
    for (w, way) in ways.iter().enumerate() {
        for s in way.coords.windows(2) {
            let mid = [
                ((u64::from(s[0][0]) + u64::from(s[1][0])) / 2) as u32,
                ((u64::from(s[0][1]) + u64::from(s[1][1])) / 2) as u32,
            ];
            // Just north of the way is its left when walking east.
            let north = [mid[0], mid[1] - 200];
            let spot = t.locate(w as u32, north).unwrap();
            let st = &t.streets[spot.street as usize];
            let a = point_at(&st.parts, &st.along, spot.offset_m - 1.0);
            let b = point_at(&st.parts, &st.along, spot.offset_m + 1.0);
            if s[1][0] > s[0][0] + 1000 {
                assert_eq!(spot.left, b[0] > a[0], "way {w}");
            }
            for p in [s[0], mid] {
                let spot = t.locate(w as u32, p).unwrap();
                assert!(spot.dist_m < 1.0, "way {w}: {spot:?}");
                let st = &t.streets[spot.street as usize];
                assert_eq!(street_key(&st.name), street_key(&way.names[0]));
                let q = point_at(&st.parts, &st.along, spot.offset_m);
                let d = dist(p, q) * st.along.mpu;
                assert!(d < 1.0, "way {w}: {d} m");
            }
        }
    }
    // Every piece is in exactly one street.
    let streets = t.piece_streets();
    assert_eq!(streets.len() as u64, t.stats.pieces);
    assert!(streets.iter().all(|&s| (s as usize) < t.streets.len()));
}

#[test]
fn sections_round_trip() {
    let (admin, ways) = (admin(), ways());
    let t = Table::build(&ways, &admin, &[], &[]);
    // Small blocks, so the cursor restarts.
    let (sstr, sgeo, sizes) = encode::sections(&t, 16).unwrap();
    assert_eq!(sizes.streets, t.streets.len() as u64);
    assert!(sizes.sgeo_blocks > 1);
    let decoded = encode::decode(&sstr, &sgeo).unwrap();
    assert_eq!(decoded.len(), t.streets.len());
    for (s, (r, parts)) in t.streets.iter().zip(&decoded) {
        assert_eq!(r.name, s.name);
        assert_eq!(r.town, s.town);
        assert_eq!(r.class, s.class);
        assert_eq!(r.admin, s.admin);
        assert_eq!(r.length_m, s.along.length_m().round() as u64);
        assert_eq!(*parts, s.parts);
        // The app measures offsets the same way.
        assert_eq!(Along::of(parts).cum, s.along.cum);
    }

    // The app's reader decodes the same streets, offsets and grid.
    let mut loose = vec![t.streets[0].parts[0][0]];
    loose.push(project(45.0, -75.0));
    let (sgrd, gsizes) = grid::section(&t, &loose, omp_core::grid::ZOOM, 64).unwrap();
    assert!(gsizes.blocks > 1 && gsizes.loose_cells == 2);
    let dir = tempfile::tempdir().unwrap();
    let pack = crate::build::test_pack(
        dir.path(),
        &[
            (encode::SSTR, &sstr),
            (encode::SGEO, &sgeo),
            (grid::SGRD, &sgrd),
        ],
    );
    let core = omp_core::street::Streets::open(pack.clone())
        .unwrap()
        .unwrap();
    let cells = omp_core::grid::Grid::open(pack).unwrap().unwrap();
    assert_eq!(cells.count() as u64, gsizes.cells);
    assert_eq!(core.count() as usize, t.streets.len());
    for (i, s) in t.streets.iter().enumerate() {
        let r = core.street(i as u32).unwrap();
        assert_eq!(
            (&r.name, &r.town, r.class, r.admin),
            (&s.name, &s.town, s.class, s.admin)
        );
        assert_eq!((&r.alt_names, &r.refs), (&s.alt_names, &s.refs));
        let g = core.geometry(i as u32).unwrap();
        assert_eq!(g.parts, s.parts);
        assert_eq!(g.cum, s.along.cum);
        for m in [0.0, 7.5, s.along.length_m() / 2.0, s.along.length_m()] {
            assert_eq!(g.point_at(m), point_at(&s.parts, &s.along, m));
        }
        for part in &s.parts {
            for p in part {
                let (x, y) = omp_core::grid::cell_xy(*p);
                assert!(cells.cell(x, y).unwrap().streets.contains(&(i as u32)));
            }
        }
    }
    let (x, y) = omp_core::grid::cell_xy(loose[1]);
    assert!(cells.cell(x, y).unwrap().loose);
}
