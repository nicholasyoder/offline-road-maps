//! Round trip through every search, address, street, admin and grid section: a test writer
//! that follows `docs/format.md` on its own (not the builder's encoders), a pack with small
//! chunks, and the readers.

use std::path::Path;
use std::sync::Arc;

use crate::addr::{self, Address, Loose, Range};
use crate::admin::{self, Area};
use crate::geo::{self, P};
use crate::grid::{self, Cell};
use crate::pack::{Info, PAGE, Pack, Writer};
use crate::search::{self, Entry, Found, Kind, NearAddress, Search};
use crate::street::{self, Geometry, STEP_M, Street, TownSource};
use crate::varint::{put, zigzag};

fn put_str(out: &mut Vec<u8>, s: &str) {
    put(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn frame(raw: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(raw, 3).unwrap()
}

/// `count nblocks (first offset length)* block*`.
fn blocked(count: usize, blocks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let frames: Vec<(u32, Vec<u8>)> = blocks.iter().map(|(f, b)| (*f, frame(b))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    let mut offset = 8 + 12 * frames.len();
    for (f, b) in &frames {
        for v in [*f, offset as u32, b.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        offset += b.len();
    }
    for (_, b) in &frames {
        out.extend_from_slice(b);
    }
    out
}

/// Metres east and south of a point near Columbus, Ohio, on the 2^6 grid.
fn at(east_m: f64, south_m: f64) -> P {
    let o = geo::project(39.96, -83.0);
    let mpu = geo::metres_per_unit(o);
    let q = |v: f64| ((v as u32) >> 6) << 6;
    [
        q(f64::from(o[0]) + east_m / mpu),
        q(f64::from(o[1]) + south_m / mpu),
    ]
}

fn square(c: P, half_m: f64) -> Vec<P> {
    let mpu = geo::metres_per_unit(c);
    let h = (half_m / mpu) as u32;
    let q = |v: u32| (v >> 6) << 6;
    vec![
        [q(c[0] - h), q(c[1] - h)],
        [q(c[0] + h), q(c[1] - h)],
        [q(c[0] + h), q(c[1] + h)],
        [q(c[0] - h), q(c[1] + h)],
    ]
}

struct Fixture {
    areas: Vec<(Area, Vec<Vec<P>>)>,
    streets: Vec<(Street, Vec<Vec<P>>)>,
    /// Sorted by street, number, suffix, offset.
    addresses: Vec<Address>,
    loose: Vec<Loose>,
    ranges: Vec<Range>,
    codes: Vec<&'static str>,
    entries: Vec<Entry>,
    towns: Vec<&'static str>,
}

fn area(
    level: u8,
    parent: Option<u32>,
    name: &str,
    abbr: &str,
    ring: Vec<P>,
) -> (Area, Vec<Vec<P>>) {
    let xs = ring.iter().map(|p| p[0]);
    let ys = ring.iter().map(|p| p[1]);
    let bbox = [
        xs.clone().min().unwrap(),
        ys.clone().min().unwrap(),
        xs.max().unwrap(),
        ys.max().unwrap(),
    ];
    let a = Area {
        level,
        parent,
        town: level == 8,
        broken: false,
        name: name.into(),
        abbr: abbr.into(),
        population: u64::from(level) * 1000,
        label: ring[0],
        bbox,
        osm_id: u64::from(level) * 7,
        alt_names: vec![("name:fr".into(), format!("{name} (fr)"))],
    };
    (a, vec![ring])
}

fn street(name: &str, town: &str, parts: Vec<Vec<P>>) -> (Street, Vec<Vec<P>>) {
    let g = Geometry::new(parts.clone());
    let s = Street {
        class: 11,
        town_source: TownSource::Addresses,
        name: name.into(),
        town: town.into(),
        admin: Some(2),
        length_m: g.length_m().round() as u64,
        alt_names: if name == "Main Street" {
            vec!["Old Main".into()]
        } else {
            Vec::new()
        },
        refs: if name == "Main Street" {
            vec!["OH 3".into()]
        } else {
            Vec::new()
        },
    };
    (s, parts)
}

fn address(street: u32, number: u32, suffix: &str, offset: i32) -> Address {
    Address {
        street,
        number,
        suffix: suffix.into(),
        offset,
        left: number % 2 == 1,
        osm: number == 103,
        approx: number == 105,
        postcode: Some(0),
        units: Vec::new(),
    }
}

fn loose(east: f64, south: f64, name: &str, number: u32) -> Loose {
    let p = at(east, south);
    Loose {
        p: [(p[0] >> 7) << 7, (p[1] >> 7) << 7],
        street: name.into(),
        number,
        suffix: if number == 9 {
            "B".into()
        } else {
            String::new()
        },
        osm: false,
        approx: number == 9,
        postcode: if number == 9 { Some(1) } else { None },
        units: if number == 9 {
            vec!["Unit 1".into()]
        } else {
            Vec::new()
        },
    }
}

fn fixture() -> Fixture {
    let centre = at(0.0, 0.0);
    let areas = vec![
        area(4, None, "Ohio", "OH", square(centre, 50_000.0)),
        area(6, Some(0), "Franklin", "", square(centre, 10_000.0)),
        area(8, Some(1), "Columbus", "", square(at(-500.0, 0.0), 2_000.0)),
        // A town in another county: its parent is not this one.
        area(
            8,
            Some(0),
            "Far Town",
            "",
            square(at(30_000.0, 0.0), 1_000.0),
        ),
    ];
    let mut streets = vec![
        // Two parts: the second starts on a fresh 5 m step past the first's end.
        street(
            "Main Street",
            "Columbus",
            vec![
                vec![at(0.0, 0.0), at(100.0, 0.0), at(200.0, 0.0)],
                vec![at(100.0, 0.0), at(100.0, 120.0)],
            ],
        ),
        street(
            "High Street",
            "Columbus",
            vec![vec![at(0.0, -50.0), at(0.0, -400.0)]],
        ),
        // One point.
        street("Stub Lane", "", vec![vec![at(500.0, 500.0)]]),
    ];
    // Enough streets for several blocks, one of them crossing a z14 cell edge.
    for i in 0..12 {
        let x = 2_000.0 + 400.0 * f64::from(i);
        streets.push(street(
            &format!("Road {i}"),
            "Columbus",
            vec![vec![at(x, 1_000.0), at(x + 300.0, 1_300.0)]],
        ));
    }
    let mut addresses = vec![
        address(0, 101, "", 2),
        address(0, 101, "A", 3),
        address(0, 103, "", 8),
        address(0, 105, "", 30),
        address(0, 7, "", 54),
        address(5, 1, "", 0),
        address(5, 3, "1/2", 10),
        address(14, 40, "", 1),
    ];
    addresses[2].units = vec!["Apt 2".into(), "Apt 10".into()];
    addresses[3].postcode = Some(1);
    addresses[5].postcode = None;
    addresses[6].postcode = None;
    addresses.sort_by(|a, b| {
        (a.street, a.number, &a.suffix, a.offset).cmp(&(b.street, b.number, &b.suffix, b.offset))
    });
    let loose = vec![
        loose(-3_000.0, -3_000.0, "Kuup Tukia", 859),
        loose(-3_010.0, -3_000.0, "Kuup Tukia", 9),
        loose(-8_000.0, 5_000.0, "RUE A", 12),
    ];
    let ranges = vec![
        Range {
            street: 1,
            start: 10,
            end: 70,
            from: 200,
            to: 298,
            parity: 2,
            left: false,
        },
        Range {
            street: 1,
            start: 70,
            end: 10,
            from: 201,
            to: 299,
            parity: 1,
            left: true,
        },
        Range {
            street: 6,
            start: 0,
            end: 50,
            from: 1,
            to: 99,
            parity: 0,
            left: true,
        },
    ];
    let entry = |kind, name: &str, p, reference, town| Entry {
        kind,
        class: 0,
        importance: 100,
        name: name.into(),
        p,
        admin: Some(2),
        town,
        reference,
        alts: Vec::new(),
    };
    let mut entries = vec![
        entry(Kind::Place, "Columbus", at(-500.0, 0.0), 3, None),
        entry(Kind::Street, "Main Street", at(100.0, 0.0), 0, Some(0)),
        entry(Kind::Street, "High Street", at(0.0, -200.0), 1, Some(0)),
        entry(Kind::Postcode, "43215", at(10.0, 10.0), 800, Some(0)),
    ];
    entries[1].alts = vec!["Old Main".into(), "OH 3".into()];
    entries[1].importance = 200;
    for i in 0..30 {
        entries.push(entry(
            Kind::Poi,
            &format!("Cafe {i}"),
            at(f64::from(i) * 10.0, 50.0),
            0,
            Some(1),
        ));
    }
    Fixture {
        areas,
        streets,
        addresses,
        loose,
        ranges,
        codes: vec!["43215", "K7L 3N6"],
        entries,
        towns: vec!["Columbus", "Grandview"],
    }
}

/// The cells a street's segments pass through, by sampling every metre (enough here).
fn street_cells(parts: &[Vec<P>]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for part in parts {
        out.push(grid::cell_xy(part[0]));
        for w in part.windows(2) {
            let n = (geo::dist(w[0], w[1]) * geo::metres_per_unit(w[0])).ceil() as u32 + 1;
            for k in 0..=n {
                let t = f64::from(k) / f64::from(n);
                let lerp =
                    |a: u32, b: u32| (f64::from(a) + t * (f64::from(b) - f64::from(a))) as u32;
                out.push(grid::cell_xy([
                    lerp(w[0][0], w[1][0]),
                    lerp(w[0][1], w[1][1]),
                ]));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Sections per format.md, `per` items to a block, so lookups cross blocks.
fn sections(f: &Fixture, per: usize) -> Vec<([u8; 4], Vec<u8>)> {
    let mut out = Vec::new();

    // SADM, SAPL
    let mut raw = Vec::new();
    put(&mut raw, f.areas.len() as u64);
    for (a, _) in &f.areas {
        raw.push(a.level);
        put(&mut raw, a.parent.map_or(0, |p| u64::from(p) + 1));
        put(&mut raw, u64::from(a.town) | u64::from(a.broken) << 1);
        put_str(&mut raw, &a.name);
        put_str(&mut raw, &a.abbr);
        put(&mut raw, a.population);
        for v in [
            a.label[0], a.label[1], a.bbox[0], a.bbox[1], a.bbox[2], a.bbox[3],
        ] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        put(&mut raw, a.osm_id);
        put(&mut raw, a.alt_names.len() as u64);
        for (k, v) in &a.alt_names {
            put_str(&mut raw, k);
            put_str(&mut raw, v);
        }
    }
    out.push((admin::SADM, frame(&raw)));
    let blocks: Vec<(u32, Vec<u8>)> = f
        .areas
        .chunks(per)
        .enumerate()
        .map(|(b, chunk)| {
            let mut raw = Vec::new();
            for (_, rings) in chunk {
                let mut cur = [0i32; 2];
                put(&mut raw, rings.len() as u64);
                for r in rings {
                    put(&mut raw, r.len() as u64);
                    for p in r {
                        let q = [(p[0] >> 6) as i32, (p[1] >> 6) as i32];
                        put(&mut raw, zigzag(q[0] - cur[0]));
                        put(&mut raw, zigzag(q[1] - cur[1]));
                        cur = q;
                    }
                }
            }
            ((b * per) as u32, raw)
        })
        .collect();
    out.push((admin::SAPL, blocked(f.areas.len(), &blocks)));

    // SSTR, SGEO
    let mut sstr = Vec::new();
    let mut sgeo = Vec::new();
    for (b, chunk) in f.streets.chunks(per).enumerate() {
        let (mut rec, mut geom) = (Vec::new(), Vec::new());
        let mut cur = [0i32; 2];
        for (s, parts) in chunk {
            // Town source 0: from addresses.
            put(&mut rec, u64::from(s.class));
            put_str(&mut rec, &s.name);
            put_str(&mut rec, &s.town);
            put(&mut rec, s.admin.map_or(0, |a| u64::from(a) + 1));
            put(&mut rec, s.length_m);
            for list in [&s.alt_names, &s.refs] {
                put(&mut rec, list.len() as u64);
                for a in list {
                    put_str(&mut rec, a);
                }
            }
            put(&mut geom, parts.len() as u64);
            for part in parts {
                put(&mut geom, part.len() as u64);
                for p in part {
                    let q = [(p[0] >> 6) as i32, (p[1] >> 6) as i32];
                    put(&mut geom, zigzag(q[0] - cur[0]));
                    put(&mut geom, zigzag(q[1] - cur[1]));
                    cur = q;
                }
            }
        }
        sstr.push(((b * per) as u32, rec));
        sgeo.push(((b * per) as u32, geom));
    }
    out.push((street::SSTR, blocked(f.streets.len(), &sstr)));
    out.push((street::SGEO, blocked(f.streets.len(), &sgeo)));

    // ASTR, ASUF, AUNI: `per` streets with addresses to a block.
    let mut runs: Vec<(u32, Vec<&Address>)> = Vec::new();
    for a in &f.addresses {
        match runs.last_mut() {
            Some((s, v)) if *s == a.street => v.push(a),
            _ => runs.push((a.street, vec![a])),
        }
    }
    let (mut astr, mut asuf, mut auni) = (Vec::new(), Vec::new(), Vec::new());
    for chunk in runs.chunks(per) {
        let first = chunk[0].0;
        let (mut a, mut sufs, mut codes, mut units) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut prev, mut ordinal) = (first, 0u64);
        let (mut ls, mut lc, mut lu) = (0, 0, 0);
        let (mut ns, mut nc) = (0, 0);
        for (s, list) in chunk {
            // The street's postcode: its commonest.
            let code = list[0].postcode;
            put(&mut a, u64::from(s - prev));
            put(&mut a, list.len() as u64);
            put(&mut a, code.map_or(0, |c| u64::from(c) + 1));
            let (mut num, mut off) = (0, 0);
            for x in list {
                let flags = u64::from(x.left) << 2 | u64::from(x.osm) << 1 | u64::from(x.approx);
                put(&mut a, u64::from(x.number - num) << 3 | flags);
                put(&mut a, zigzag(x.offset - off));
                (num, off) = (x.number, x.offset);
                if !x.suffix.is_empty() {
                    put(&mut sufs, ordinal - ls);
                    put_str(&mut sufs, &x.suffix);
                    (ls, ns) = (ordinal, ns + 1);
                }
                if x.postcode != code {
                    put(&mut codes, ordinal - lc);
                    put(&mut codes, x.postcode.map_or(0, |c| u64::from(c) + 1));
                    (lc, nc) = (ordinal, nc + 1);
                }
                if !x.units.is_empty() {
                    put(&mut units, ordinal - lu);
                    put(&mut units, x.units.len() as u64);
                    for u in &x.units {
                        put_str(&mut units, u);
                    }
                    lu = ordinal;
                }
                ordinal += 1;
            }
            prev = *s;
        }
        let mut s = Vec::new();
        put(&mut s, ns);
        s.extend_from_slice(&sufs);
        put(&mut s, nc);
        s.extend_from_slice(&codes);
        astr.push((first, a));
        asuf.push((first, s));
        auni.push((first, units));
    }
    out.push((addr::ASTR, blocked(f.addresses.len(), &astr)));
    out.push((addr::ASUF, blocked(0, &asuf)));
    out.push((addr::AUNI, blocked(0, &auni)));

    // ALSE: one cell per block.
    let mut loose: Vec<&Loose> = f.loose.iter().collect();
    loose.sort_by_key(|l| (addr::cell_of(l.p), geo::morton(l.p), l.number));
    let mut alse = Vec::new();
    for group in loose.chunk_by(|a, b| addr::cell_of(a.p) == addr::cell_of(b.p)) {
        let mut names: Vec<&str> = group.iter().map(|l| l.street.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let mut raw = Vec::new();
        put(&mut raw, names.len() as u64);
        for n in &names {
            put_str(&mut raw, n);
        }
        let mut cur = [0i32; 2];
        for l in group {
            let q = [(l.p[0] >> 7) as i32, (l.p[1] >> 7) as i32];
            put(&mut raw, zigzag(q[0] - cur[0]));
            put(&mut raw, zigzag(q[1] - cur[1]));
            cur = q;
            put(
                &mut raw,
                names.binary_search(&l.street.as_str()).unwrap() as u64,
            );
            put(
                &mut raw,
                u64::from(l.number) << 2 | u64::from(l.osm) << 1 | u64::from(l.approx),
            );
            put_str(&mut raw, &l.suffix);
            put(&mut raw, l.postcode.map_or(0, |c| u64::from(c) + 1));
            put(&mut raw, l.units.len() as u64);
            for u in &l.units {
                put_str(&mut raw, u);
            }
        }
        alse.push((addr::cell_of(group[0].p), raw));
    }
    out.push((addr::ALSE, blocked(f.loose.len(), &alse)));

    // ARNG: one street per block.
    let mut arng = Vec::new();
    for group in f.ranges.chunk_by(|a, b| a.street == b.street) {
        let mut raw = Vec::new();
        let mut start = 0;
        for r in group {
            put(&mut raw, 0);
            put(&mut raw, zigzag(r.start - start));
            put(&mut raw, zigzag(r.end - r.start));
            put(&mut raw, u64::from(r.from));
            put(&mut raw, zigzag(r.to as i32 - r.from as i32));
            put(&mut raw, u64::from(r.parity) << 1 | u64::from(r.left));
            start = r.start;
        }
        arng.push((group[0].street, raw));
    }
    out.push((addr::ARNG, blocked(f.ranges.len(), &arng)));

    let codes: Vec<(u32, Vec<u8>)> = f
        .codes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut raw = Vec::new();
            put_str(&mut raw, c);
            (i as u32, raw)
        })
        .collect();
    out.push((addr::APCD, blocked(f.codes.len(), &codes)));

    // SGRD: cells with streets or unattached points, `per` cells to a block.
    let mut cells: Vec<(u32, Cell)> = Vec::new();
    let mut add = |xy: (u32, u32), street: Option<u32>, loose: bool| {
        let k = grid::key(xy.0, xy.1);
        let i = match cells.iter().position(|c| c.0 == k) {
            Some(i) => i,
            None => {
                cells.push((k, Cell::default()));
                cells.len() - 1
            }
        };
        cells[i].1.streets.extend(street);
        cells[i].1.loose |= loose;
    };
    for (s, (_, parts)) in f.streets.iter().enumerate() {
        for xy in street_cells(parts) {
            add(xy, Some(s as u32), false);
        }
    }
    for l in &f.loose {
        add(grid::cell_xy(l.p), None, true);
    }
    cells.sort_by_key(|c| c.0);
    let sgrd: Vec<(u32, Vec<u8>)> = cells
        .chunks(per)
        .map(|chunk| {
            let mut raw = Vec::new();
            let (mut prev, mut first) = (chunk[0].0, 0u32);
            for (k, c) in chunk {
                put(&mut raw, u64::from(k - prev));
                put(&mut raw, (c.streets.len() as u64) << 1 | u64::from(c.loose));
                if let Some(&s) = c.streets.first() {
                    put(&mut raw, zigzag(s as i32 - first as i32));
                    first = s;
                    for w in c.streets.windows(2) {
                        put(&mut raw, u64::from(w[1] - w[0]));
                    }
                }
                prev = *k;
            }
            (chunk[0].0, raw)
        })
        .collect();
    out.push((grid::SGRD, blocked(cells.len(), &sgrd)));

    // SENT, STWN, SFST, SPST
    let blocks: Vec<(u32, Vec<u8>)> = f
        .entries
        .chunks(per)
        .enumerate()
        .map(|(b, chunk)| {
            let mut raw = Vec::new();
            let mut cur = [0i32; 2];
            for e in chunk {
                put(&mut raw, u64::from(e.class) << 3 | e.kind as u64);
                raw.push(e.importance);
                put_str(&mut raw, &e.name);
                let q = [(e.p[0] >> 6) as i32, (e.p[1] >> 6) as i32];
                put(&mut raw, zigzag(q[0] - cur[0]));
                put(&mut raw, zigzag(q[1] - cur[1]));
                cur = q;
                put(&mut raw, e.admin.map_or(0, |a| u64::from(a) + 1));
                put(&mut raw, e.town.map_or(0, |t| u64::from(t) + 1));
                put(&mut raw, e.reference);
                put(&mut raw, e.alts.len() as u64);
                for a in &e.alts {
                    put_str(&mut raw, a);
                }
            }
            ((b * per) as u32, raw)
        })
        .collect();
    out.push((search::SENT, blocked(f.entries.len(), &blocks)));
    let mut raw = Vec::new();
    put(&mut raw, f.towns.len() as u64);
    for t in &f.towns {
        put_str(&mut raw, t);
    }
    out.push((search::STWN, frame(&raw)));
    let mut postings: std::collections::BTreeMap<String, Vec<u32>> = Default::default();
    for (i, e) in f.entries.iter().enumerate() {
        for name in std::iter::once(&e.name).chain(&e.alts) {
            for t in crate::normalize::search_tokens(name) {
                let v = postings.entry(t).or_default();
                if v.last() != Some(&(i as u32)) {
                    v.push(i as u32);
                }
            }
        }
    }
    let (mut spst, mut fst) = (Vec::new(), fst::MapBuilder::memory());
    for (token, ids) in &postings {
        fst.insert(token, spst.len() as u64).unwrap();
        let mut deltas = Vec::new();
        let mut prev = 0;
        for &id in ids {
            put(&mut deltas, u64::from(id - prev));
            prev = id;
        }
        put(&mut spst, ids.len() as u64);
        put(&mut spst, ids.len() as u64);
        put(&mut spst, deltas.len() as u64);
        spst.extend_from_slice(&deltas);
    }
    out.push((search::SFST, fst.into_inner().unwrap()));
    out.push((search::SPST, spst));
    out
}

fn write_pack(dir: &Path, sections: &[([u8; 4], Vec<u8>)]) -> Arc<Pack> {
    // Two pages per chunk, so sections land in different chunk files.
    let mut w = Writer::create(dir, "t", 2 * PAGE).unwrap();
    for (id, b) in sections {
        w.section(*id, b).unwrap();
    }
    let info = Info {
        pack_id: [7; 16],
        bbox: [0; 4],
        build_time: 0,
        name: "Test".into(),
        snapshot: String::new(),
    };
    let paths = w.finish(&info).unwrap();
    let pack = Pack::open(&paths[0]).unwrap();
    assert!(pack.chunks() > 4, "{} chunks", pack.chunks());
    Arc::new(pack)
}

#[test]
fn readers_round_trip() {
    let f = fixture();
    for per in [1, 2, 1000] {
        let dir = tempfile::tempdir().unwrap();
        let pack = write_pack(dir.path(), &sections(&f, per));
        let s = Search::open(pack).unwrap().expect("a pack with search");

        // Admin: the table, polygons and chains.
        let areas: Vec<&Area> = f.areas.iter().map(|a| &a.0).collect();
        assert_eq!(s.admin.areas().iter().collect::<Vec<_>>(), areas);
        for (i, (_, rings)) in f.areas.iter().enumerate() {
            assert_eq!(&s.admin.polygon(i as u32).unwrap(), rings);
        }
        assert_eq!(s.admin.chain(at(-500.0, 100.0)).unwrap(), [0, 1, 2]);
        assert_eq!(s.admin.chain(at(5_000.0, 0.0)).unwrap(), [0, 1]);
        assert_eq!(s.admin.chain(at(30_000.0, 0.0)).unwrap(), [0, 3]);
        assert_eq!(s.admin.chain(at(40_000.0, 0.0)).unwrap(), [0]);
        assert!(s.admin.chain(at(60_000.0, 0.0)).unwrap().is_empty());
        assert_eq!(s.admin.parents(2), [2, 1, 0]);

        // Streets: records, geometry and offsets.
        assert_eq!(s.streets.count() as usize, f.streets.len());
        for (i, (rec, parts)) in f.streets.iter().enumerate() {
            assert_eq!(&s.streets.street(i as u32).unwrap(), rec);
            assert_eq!(&s.streets.geometry(i as u32).unwrap().parts, parts);
        }
        assert!(s.streets.street(f.streets.len() as u32).is_err());
        let mut seen = 0;
        s.streets
            .for_each(|i, rec, g| {
                assert_eq!(rec, f.streets[i as usize].0);
                assert_eq!(g.parts, f.streets[i as usize].1);
                seen += 1;
            })
            .unwrap();
        assert_eq!(seen, f.streets.len());
        let main = s.streets.geometry(0).unwrap();
        // Part 0 is 200 m; part 1 starts at 5 × (40 + 1) = 205 m.
        assert!((main.cum[0][2] - 200.0).abs() < 0.5, "{:?}", main.cum);
        assert_eq!(main.cum[1][0], 5.0 * ((main.cum[0][2] / 5.0).ceil() + 1.0));

        // Addresses: runs with suffixes, postcodes and units.
        let a = s.addresses.as_ref().unwrap();
        assert_eq!(a.counts(), [8, 3, 3, 2]);
        let run0: Vec<Address> = f
            .addresses
            .iter()
            .filter(|x| x.street == 0)
            .cloned()
            .collect();
        assert_eq!(a.run(0).unwrap(), run0);
        assert_eq!(a.run(5).unwrap().len(), 2);
        assert_eq!(a.run(5).unwrap()[1].suffix, "1/2");
        assert_eq!(a.run(14).unwrap()[0].number, 40);
        assert!(a.run(1).unwrap().is_empty() && a.run(9).unwrap().is_empty());
        let mut all = Vec::new();
        a.for_each(|x| all.push(x.clone())).unwrap();
        assert_eq!(all, f.addresses);
        assert_eq!(a.ranges(1).unwrap(), f.ranges[..2]);
        assert_eq!(a.ranges(6).unwrap(), f.ranges[2..]);
        assert!(a.ranges(0).unwrap().is_empty());
        assert_eq!(a.postcode(1).unwrap(), "K7L 3N6");
        assert!(a.postcode(2).is_err());
        let ku = a.loose_in(addr::cell_of(f.loose[0].p)).unwrap();
        assert_eq!(ku.len(), 2);
        assert!(ku.contains(&f.loose[1]));
        assert_eq!(
            a.loose_in(addr::cell_of(f.loose[2].p)).unwrap(),
            [f.loose[2].clone()]
        );
        assert!(a.loose_in(addr::cell_of(at(0.0, 0.0))).unwrap().is_empty());

        // Grid: every street is in the cells it passes through; empty cells are empty.
        for (i, (_, parts)) in f.streets.iter().enumerate() {
            for (x, y) in street_cells(parts) {
                assert!(s.grid.cell(x, y).unwrap().streets.contains(&(i as u32)));
            }
        }
        let (lx, ly) = grid::cell_xy(f.loose[2].p);
        assert_eq!(
            s.grid.cell(lx, ly).unwrap(),
            Cell {
                streets: Vec::new(),
                loose: true
            }
        );
        let far = grid::cell_xy(at(-20_000.0, -20_000.0));
        assert_eq!(s.grid.cell(far.0, far.1).unwrap(), Cell::default());
        assert_eq!(s.grid.cell(1 << 14, 0).unwrap(), Cell::default());
        let mut cells = 0;
        s.grid.for_each(|_, _| cells += 1).unwrap();
        assert_eq!(cells, s.grid.count());

        // Entries, towns and postings.
        assert_eq!(s.index.count() as usize, f.entries.len());
        for (i, e) in f.entries.iter().enumerate() {
            assert_eq!(&s.index.entry(i as u32).unwrap(), e);
        }
        assert_eq!(s.index.towns(), f.towns);
        let cafe = s.index.postings("cafe").unwrap().unwrap();
        assert_eq!(cafe.ids, (4..34).collect::<Vec<u32>>());
        assert_eq!((cafe.total, cafe.stored), (30, 30));
        let offset = s.index.tokens().get("cafe").unwrap();
        assert_eq!(s.index.postings_at(offset, 3).unwrap().ids, [4, 5, 6]);
        let main_ids = s.index.postings("main").unwrap().unwrap().ids;
        assert_eq!(main_ids, [1]);
        assert_eq!(
            s.index.postings("old").unwrap().unwrap().ids,
            [1],
            "alt name"
        );
        assert_eq!(s.index.postings("43215").unwrap().unwrap().ids, [3]);
        assert_eq!(s.index.postings("nowhere").unwrap(), None);

        // Locating a number: exact points (suffix first), ranges, nothing.
        let at101 = s.locate(0, 101, "a").unwrap();
        assert_eq!(at101.len(), 2);
        let Found::Point(first) = &at101[0].found else {
            panic!()
        };
        assert_eq!(first.suffix, "A");
        assert_eq!(at101[0].p, main.point_at(3.0 * STEP_M));
        let at105 = s.locate(0, 105, "").unwrap();
        assert!(at105[0].approx, "an NAR blockface point");
        let r = s.locate(1, 250, "").unwrap();
        assert_eq!(r.len(), 1);
        assert!(r[0].approx);
        assert_eq!(r[0].found, Found::Range(f.ranges[0]));
        let high = s.streets.geometry(1).unwrap();
        let want = high.point_at(f.ranges[0].offset_of(250) * STEP_M);
        assert_eq!(r[0].p, want);
        assert!(s.locate(1, 251, "").unwrap()[0].found == Found::Range(f.ranges[1]));
        assert!(s.locate(1, 300, "").unwrap().is_empty());
        assert!(s.locate(2, 1, "").unwrap().is_empty());

        // Near a point: streets and addresses within a radius, nearest first.
        let near = s.streets_near(at(50.0, 3.0), 30.0).unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].0, 0);
        assert!((near[0].1.offset_m - 50.0).abs() < 0.5 && (near[0].1.dist_m - 3.0).abs() < 0.5);
        let both = s.streets_near(at(10.0, -40.0), 60.0).unwrap();
        assert_eq!(both.iter().map(|n| n.0).collect::<Vec<_>>(), [1, 0]);
        let found = s.addresses_near(at(15.0, 0.0), 30.0).unwrap();
        let numbers: Vec<u32> = found
            .iter()
            .map(|n| match &n.address {
                NearAddress::Street(a) => a.number,
                NearAddress::Loose(l) => l.number,
            })
            .collect();
        assert_eq!(
            numbers,
            [101, 101, 103],
            "15 m: 101A, then 101 at 10 m, 103 at 40 m"
        );
        let lp = f.loose[1].p;
        let loose_near = s.addresses_near(lp, 20.0).unwrap();
        assert_eq!(loose_near.len(), 2);
        assert_eq!(
            loose_near[0].address,
            NearAddress::Loose(f.loose[1].clone())
        );
        assert!(s.heap_bytes() > 0 && s.bytes_read() > 0);
    }
}

#[test]
fn packs_without_search_open_without_it() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let all = sections(&f, 4);
    // Without the grid there is no search; without addresses there is.
    let no_grid: Vec<_> = all.iter().filter(|s| s.0 != grid::SGRD).cloned().collect();
    assert!(
        Search::open(write_pack(dir.path(), &no_grid))
            .unwrap()
            .is_none()
    );
    let dir = tempfile::tempdir().unwrap();
    let no_addr: Vec<_> = all.iter().filter(|s| s.0[0] != b'A').cloned().collect();
    let s = Search::open(write_pack(dir.path(), &no_addr))
        .unwrap()
        .unwrap();
    assert!(s.addresses.is_none());
    assert!(s.locate(0, 101, "").unwrap().is_empty());
    assert!(s.addresses_near(at(0.0, 0.0), 50.0).unwrap().is_empty());
}
