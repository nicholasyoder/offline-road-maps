//! `SADM` (admin table) and `SAPL` (admin polygons), as in `docs/format.md`.

use anyhow::Result;
use omp_core::varint::{put, put_str, zigzag};
use serde::Serialize;

use super::{Admin, PACK_TOLERANCE_M, abbreviation, index};
use crate::geom::BBox;

pub const SADM: [u8; 4] = *b"SADM";
pub const SAPL: [u8; 4] = *b"SAPL";
const LEVEL: i32 = 19;
/// Uncompressed bytes per `SAPL` block, about. `SAPL` has the street sections' layout.
const BLOCK: usize = 64 << 10;
/// `SAPL` coordinates are in units of 2^6 world units (about 0.6 m at the equator).
pub const SAPL_SHIFT: u32 = 6;

pub const FLAG_TOWN: u64 = 1;
pub const FLAG_BROKEN: u64 = 2;

#[derive(Serialize, Default)]
pub struct Sizes {
    pub areas: u64,
    pub sadm_raw: u64,
    pub sadm: u64,
    pub sapl_raw: u64,
    pub sapl: u64,
    pub sapl_blocks: u64,
    /// Per area, the vertices stored in `SAPL`.
    #[serde(skip)]
    pub pack_vertices: Vec<u64>,
}

/// The two sections' bytes and their sizes.
pub fn sections(admin: &Admin) -> Result<(Vec<u8>, Vec<u8>, Sizes)> {
    let mut raw = Vec::new();
    put(&mut raw, admin.areas.len() as u64);
    for (i, a) in admin.areas.iter().enumerate() {
        raw.push(a.level);
        put(&mut raw, admin.parents[i].map_or(0, |p| u64::from(p) + 1));
        let flags =
            if admin.towns[i] { FLAG_TOWN } else { 0 } | if a.broken { FLAG_BROKEN } else { 0 };
        put(&mut raw, flags);
        put_str(&mut raw, &a.name);
        put_str(&mut raw, a.iso.as_deref().map_or("", abbreviation));
        put(&mut raw, a.population.unwrap_or(0));
        let b = BBox::of(a.rings.iter().flatten().copied());
        for v in [admin.labels[i], b.min, b.max].into_iter().flatten() {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        put(&mut raw, a.osm_id as u64);
        put(&mut raw, a.alt_names.len() as u64);
        for (k, v) in &a.alt_names {
            put_str(&mut raw, k);
            put_str(&mut raw, v);
        }
    }
    let sadm = zstd::bulk::compress(&raw, LEVEL)?;
    let mut sizes = Sizes {
        areas: admin.areas.len() as u64,
        sadm_raw: raw.len() as u64,
        sadm: sadm.len() as u64,
        ..Default::default()
    };

    let polys: Vec<Vec<u8>> = {
        use rayon::prelude::*;
        admin
            .areas
            .par_iter()
            .map(|a| {
                let rings = index::simplify_rings(&a.rings, PACK_TOLERANCE_M);
                let mut out = Vec::new();
                put(&mut out, rings.len() as u64);
                let mut cur = [0i32; 2];
                for r in &rings {
                    put(&mut out, r.len() as u64);
                    for p in r {
                        let q = [(p[0] >> SAPL_SHIFT) as i32, (p[1] >> SAPL_SHIFT) as i32];
                        put(&mut out, zigzag(q[0] - cur[0]));
                        put(&mut out, zigzag(q[1] - cur[1]));
                        cur = q;
                    }
                }
                // The vertex count rides along at the end, for the report.
                let n: usize = rings.iter().map(Vec::len).sum();
                out.extend_from_slice(&(n as u64).to_le_bytes());
                out
            })
            .collect()
    };
    let mut blocks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut first = 0u32;
    for (i, p) in polys.iter().enumerate() {
        let (body, n) = p.split_at(p.len() - 8);
        sizes
            .pack_vertices
            .push(u64::from_le_bytes(n.try_into().unwrap()));
        if !cur.is_empty() && cur.len() + body.len() > BLOCK {
            blocks.push((first, std::mem::take(&mut cur)));
            first = i as u32;
        }
        cur.extend_from_slice(body);
        sizes.sapl_raw += body.len() as u64;
    }
    if !cur.is_empty() {
        blocks.push((first, cur));
    }
    let (sapl, nblocks) = crate::street::encode::blocked(admin.areas.len(), blocks)?;
    sizes.sapl = sapl.len() as u64;
    sizes.sapl_blocks = nblocks;
    Ok((sadm, sapl, sizes))
}

#[cfg(test)]
mod tests {
    use omp_core::varint::Reader;

    use super::*;
    use crate::admin::read::{AdminData, Area, Place, PlaceClass, ReadStats};
    use crate::geom::{P, project};

    fn area(id: i64, level: u8, name: &str, iso: Option<&str>, sw: P, ne: P) -> Area {
        Area {
            osm_id: id,
            level,
            name: name.into(),
            alt_names: vec![("name:fr".into(), format!("{name} (fr)"))],
            iso: iso.map(Into::into),
            population: Some(1000 * id as u64),
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

    fn string(r: &mut Reader) -> String {
        let n = r.count().unwrap();
        String::from_utf8(r.bytes(n).unwrap().to_vec()).unwrap()
    }

    #[test]
    fn sections_decode_as_documented() {
        let (sw, ne) = (project(38.5, -85.0), project(42.0, -80.5));
        let (tsw, tne) = (project(39.9, -83.1), project(40.1, -82.9));
        let admin = Admin::build(AdminData {
            areas: vec![
                area(3, 8, "Columbus", None, tsw, tne),
                area(2, 4, "Ohio", Some("US-OH"), sw, ne),
                area(
                    4,
                    7,
                    "Liberty Township",
                    None,
                    project(39.3, -84.5),
                    project(39.4, -84.4),
                ),
            ],
            places: vec![Place {
                osm_id: 9,
                class: PlaceClass::Town,
                name: "Princeton".into(),
                p: project(39.35, -84.45),
            }],
            stats: ReadStats::default(),
        });
        // Ordered by level: Ohio, the township, Columbus; both inside Ohio.
        let names: Vec<&str> = admin.areas.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Ohio", "Liberty Township", "Columbus"]);
        assert_eq!(admin.parents, [None, Some(0), Some(0)]);
        assert_eq!(admin.towns, [false, false, true]);
        assert_eq!(admin.chain(project(40.0, -83.0)), [0, 2]);
        assert_eq!(admin.chain(project(39.35, -84.45)), [0, 1]);
        assert!(admin.chain(project(45.0, -83.0)).is_empty());

        let (sadm, sapl, sizes) = sections(&admin).unwrap();
        assert_eq!(sizes.areas, 3);
        let raw = zstd::stream::decode_all(&sadm[..]).unwrap();
        let mut r = Reader::new(&raw);
        assert_eq!(r.count().unwrap(), 3);
        let mut decoded = Vec::new();
        for _ in 0..3 {
            let level = r.bytes(1).unwrap()[0];
            let parent = r.varint().unwrap();
            let flags = r.varint().unwrap();
            let name = string(&mut r);
            let abbr = string(&mut r);
            let population = r.varint().unwrap();
            let fixed = r.bytes(24).unwrap();
            let label = [
                u32::from_le_bytes(fixed[0..4].try_into().unwrap()),
                u32::from_le_bytes(fixed[4..8].try_into().unwrap()),
            ];
            let osm_id = r.varint().unwrap();
            let alts = r.count().unwrap();
            let alt: Vec<(String, String)> = (0..alts)
                .map(|_| (string(&mut r), string(&mut r)))
                .collect();
            decoded.push((
                level, parent, flags, name, abbr, population, label, osm_id, alt,
            ));
        }
        assert!(r.at_end());
        assert_eq!(decoded[0].4, "OH");
        assert_eq!(decoded[2].1, 1, "Columbus's parent is area 0, stored as 1");
        assert_eq!(decoded[2].2, FLAG_TOWN);
        assert_eq!((decoded[2].5, decoded[2].7), (3000, 3));
        assert_eq!(decoded[2].8, [("name:fr".into(), "Columbus (fr)".into())]);
        assert_eq!(admin.chain(decoded[2].6), [0, 2], "the label lies inside");

        // The app's reader: the table, the polygons and the chains agree.
        let dir = tempfile::tempdir().unwrap();
        let pack = crate::build::test_pack(dir.path(), &[(SADM, &sadm), (SAPL, &sapl)]);
        let core = omp_core::admin::Admin::open(pack).unwrap().unwrap();
        assert_eq!(core.areas().len(), 3);
        assert_eq!(core.areas()[0].abbr, "OH");
        assert_eq!(core.areas()[2].parent, Some(0));
        assert!(core.areas()[2].town);
        for (i, a) in admin.areas.iter().enumerate() {
            let rings = core.polygon(i as u32).unwrap();
            let want: Vec<P> = a.rings[0]
                .iter()
                .map(|p| {
                    [
                        p[0] >> SAPL_SHIFT << SAPL_SHIFT,
                        p[1] >> SAPL_SHIFT << SAPL_SHIFT,
                    ]
                })
                .collect();
            assert_eq!(rings, [want]);
        }
        for p in [
            project(40.0, -83.0),
            project(39.35, -84.45),
            project(45.0, -83.0),
        ] {
            assert_eq!(core.chain(p).unwrap(), admin.chain(p));
        }
    }
}
