use super::encode::{decode_entries, decode_towns, lookup, sections};
use super::*;
use crate::geom::project;

fn entry(kind: Kind, importance: u8, name: &str, lat: f64, lon: f64, town: u32) -> Entry {
    Entry {
        kind,
        class: 3,
        importance,
        name: name.into(),
        alts: Vec::new(),
        p: project(lat, lon),
        admin: Some(7),
        town,
        reference: 42,
    }
}

/// A small index: three "Main" streets, a city, a POI with an alternative name, a
/// postcode, and streets whose every token is generic.
fn sample() -> Index {
    let mut cafe = entry(Kind::Poi, 80, "Tim Hortons", 44.23, -76.48, 1);
    cafe.alts = vec!["Tim's".into()];
    let entries = vec![
        entry(Kind::Street, 85, "Main St", 39.96, -83.0, 2),
        entry(Kind::Street, 85, "Main Street", 41.5, -81.7, 0),
        entry(Kind::Street, 120, "N Main St", 39.1, -84.5, 2),
        entry(Kind::Place, 190, "Columbus", 39.96, -83.0, 0),
        cafe,
        entry(
            Kind::Postcode,
            POSTCODE_IMPORTANCE,
            "K7L 3N6",
            44.23,
            -76.5,
            1,
        ),
        entry(Kind::Street, 85, "North Street", 40.0, -82.0, 2),
        entry(Kind::Street, 85, "Avenue Road", 43.7, -79.4, 0),
        entry(Kind::Street, 85, "Elm Street", 40.1, -82.1, 0),
    ];
    let towns = vec!["Kingston".into(), "Columbus".into()];
    // "street" (5 entries) and "north" (2) are over the threshold of 1; "road" and
    // "avenue" are not.
    index(entries, towns, Stats::default(), Instant::now(), 1)
}

#[test]
fn ids_run_by_importance_bucket() {
    let ix = sample();
    let buckets: Vec<u8> = ix.entries.iter().map(|e| bucket(e.importance)).collect();
    assert!(buckets.windows(2).all(|w| w[0] >= w[1]), "{buckets:?}");
    assert_eq!(ix.entries[0].name, "Columbus");
    // Towns sorted, references renumbered with them.
    assert_eq!(ix.towns, ["Columbus", "Kingston"]);
    let cafe = ix.entries.iter().find(|e| e.name == "Tim Hortons").unwrap();
    assert_eq!(ix.towns[cafe.town as usize - 1], "Kingston");
}

#[test]
fn sections_round_trip() {
    let ix = sample();
    for block in [64, BLOCK_TEST] {
        let (s, sizes) = sections(&ix, block).unwrap();
        assert_eq!(sizes.entries, ix.entries.len() as u64);
        let got = decode_entries(&s.sent).unwrap();
        assert_eq!(got.len(), ix.entries.len());
        for (d, e) in got.iter().zip(&ix.entries) {
            assert_eq!(
                (
                    d.kind,
                    d.class,
                    d.importance,
                    &d.name,
                    d.admin,
                    d.town,
                    d.reference,
                    &d.alts
                ),
                (
                    e.kind,
                    e.class,
                    e.importance,
                    &e.name,
                    e.admin,
                    e.town,
                    e.reference,
                    &e.alts
                )
            );
            // Points are stored at 2^6 units.
            assert_eq!(d.p, [e.p[0] >> 6 << 6, e.p[1] >> 6 << 6]);
        }
        assert_eq!(decode_towns(&s.stwn).unwrap(), ix.towns);
        for (token, ids, total) in &ix.postings {
            let (got, t) = lookup(&s.sfst, &s.spst, token).unwrap().unwrap();
            assert_eq!((&got, t), (ids, *total), "{token}");
        }
        assert_eq!(lookup(&s.sfst, &s.spst, "nowhere").unwrap(), None);

        // The app's reader agrees.
        let dir = tempfile::tempdir().unwrap();
        let pack = crate::build::test_pack(
            dir.path(),
            &[
                (encode::SENT, &s.sent),
                (encode::STWN, &s.stwn),
                (encode::SFST, &s.sfst),
                (encode::SPST, &s.spst),
            ],
        );
        let core = omp_core::search::Index::open(pack).unwrap().unwrap();
        assert_eq!(core.count() as usize, got.len());
        assert_eq!(core.towns(), ix.towns);
        for (i, d) in got.iter().enumerate() {
            let e = core.entry(i as u32).unwrap();
            assert_eq!(e.kind as u8, d.kind as u8);
            assert_eq!(
                (e.class, e.importance, &e.name, e.p),
                (d.class, d.importance, &d.name, d.p)
            );
            assert_eq!(
                (e.admin, e.town, e.reference, &e.alts),
                (d.admin, d.town.checked_sub(1), d.reference, &d.alts)
            );
        }
        for (token, ids, total) in &ix.postings {
            let p = core.postings(token).unwrap().unwrap();
            assert_eq!((&p.ids, p.total), (ids, *total), "{token}");
        }
    }
}

const BLOCK_TEST: usize = 1 << 20;

#[test]
fn generic_tokens_list_only_entries_with_nothing_else() {
    let ix = sample();
    let (s, _) = sections(&ix, 64).unwrap();
    let names = |token: &str| -> (Vec<String>, u64) {
        let (ids, total) = lookup(&s.sfst, &s.spst, token).unwrap().unwrap();
        let mut n: Vec<String> = ids
            .iter()
            .map(|&i| ix.entries[i as usize].name.clone())
            .collect();
        n.sort();
        (n, total)
    };
    // "street": five entries hold it, only "North Street" has no other token kept.
    assert_eq!(names("street"), (vec!["North Street".into()], 5));
    assert_eq!(
        names("north"),
        (vec!["North Street".into()], 2),
        "N Main St is found by main"
    );
    assert_eq!(names("main").0, ["Main St", "Main Street", "N Main St"]);
    // Under the threshold, generic tokens keep all their entries.
    assert_eq!(names("road"), (vec!["Avenue Road".into()], 1));
    assert_eq!(names("tim").0, ["Tim Hortons"]);
    assert_eq!(names("k7l3n6").0, ["K7L 3N6"]);
    let stopped: Vec<&str> = ix.stats.stopped.iter().map(|s| s.0.as_str()).collect();
    assert_eq!(stopped, ["street", "north"]);
}

#[test]
fn importance_orders_kinds_sensibly() {
    assert!(place_importance("city", Some(900_000)) > place_importance("town", Some(20_000)));
    assert!(place_importance("city", None) > place_importance("village", Some(5_000)));
    assert_eq!(place_importance("country", Some(300_000_000)), 255);
    assert!(admin_importance(4, false, None) > admin_importance(6, false, None));
    assert!(admin_importance(8, true, None) > admin_importance(8, false, None));
    let motorway = HIGHWAYS.iter().position(|h| *h == "motorway").unwrap() as u8;
    let residential = HIGHWAYS.iter().position(|h| *h == "residential").unwrap() as u8;
    assert!(street_importance(motorway, 100.0) > street_importance(residential, 20_000.0));
    assert!(street_importance(residential, 4_000.0) > street_importance(residential, 200.0));
    assert!(poi_importance("airport") > poi_importance("parking"));
    assert!(feature_importance(4) > feature_importance(12));
}

#[test]
fn alternative_names_drop_repeats() {
    let alts = distinct_alts(
        "Montréal",
        [
            "Montreal".into(),
            "Ville-Marie".into(),
            "Montréal".into(),
            "Ville-Marie".into(),
        ],
    );
    assert_eq!(alts, ["Ville-Marie"]);
}

/// The same features collected over one band or over several, in another order, give the
/// same search sections.
#[test]
fn entries_do_not_depend_on_the_bands() {
    use crate::admin::read::{AdminData, ReadStats};
    use crate::pbf::features::Feature;

    let wl = Whitelist::parse(omp_core::whitelist::DEFAULT).unwrap();
    let admin = Admin::build(AdminData {
        areas: Vec::new(),
        places: Vec::new(),
        stats: ReadStats::default(),
    });
    let feature = |layer: &str, id: i64, class: u16, name: &str| {
        let l = wl.layer_index(layer).unwrap();
        let name_prop = wl.layers[l].props.iter().position(|p| p == "name").unwrap();
        Feature {
            id,
            layer: l as u8,
            class,
            geom: Geometry::Point(project(40.0, -83.0)),
            props: vec![(name_prop as u8, name.into())],
            minzoom: None,
        }
    };
    // Pairs with the same name, class and point, so their entries sort the same; the first
    // of each has an alternative name.
    let places = [
        feature("places", 4, 3, "Springfield"),
        feature("places", 8, 3, "Springfield"),
    ];
    let pois = [
        feature("pois", 12, 1, "Main Library"),
        feature("pois", 16, 1, "Main Library"),
    ];
    let kept: HashMap<i64, Vec<(String, String)>> = [
        (4, vec![("old_name".into(), "Old Springfield".into())]),
        (12, vec![("alt_name".into(), "Carnegie Library".into())]),
    ]
    .into();
    let index = |bands: &[Vec<&Feature>]| {
        let mut c = Collector::new(&wl);
        for band in bands {
            let mut band: Vec<Feature> = band
                .iter()
                .map(|f| Feature {
                    props: f.props.clone(),
                    geom: f.geom.clone(),
                    ..**f
                })
                .collect();
            band.sort_unstable_by_key(|f| (f.layer, f.id, f.class));
            c.add(&band, &kept);
        }
        let index = Builder::new().finish(&admin, &wl, c, &[], &[]).unwrap();
        sections(&index, 16 << 10).unwrap().0.list()
    };
    let all: Vec<&Feature> = places.iter().chain(&pois).collect();
    let one = index(&[all]);
    // Band 0 has the second of each pair, band 1 the first; band 1 also has band 0's again.
    let two = index(&[
        vec![&places[1], &pois[1]],
        vec![&places[0], &places[1], &pois[0]],
    ]);
    assert_eq!(one, two);
}
