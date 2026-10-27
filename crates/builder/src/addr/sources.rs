//! Readers for the non-OSM point sources: OpenAddresses GeoJSON, StatCan NAR and ODA CSVs.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{Raw, Src};
use crate::geom::{self, P};

#[derive(Deserialize)]
struct OaFeature {
    properties: OaProps,
    geometry: Option<Value>,
}

#[derive(Deserialize)]
struct OaProps {
    #[serde(default)]
    number: Value,
    #[serde(default)]
    street: Value,
    #[serde(default)]
    unit: Value,
    #[serde(default)]
    city: Value,
    #[serde(default)]
    postcode: Value,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn point(geometry: &Option<Value>) -> Option<P> {
    let g = geometry.as_ref()?;
    if g["type"] != "Point" {
        return None;
    }
    let c = g["coordinates"].as_array()?;
    let (lon, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
        .then(|| geom::project(lat, lon))
}

/// One gzipped GeoJSON-lines file per path. Lines that do not parse, and features with no
/// point, are skipped.
pub fn openaddresses(files: &[std::path::PathBuf]) -> Result<Vec<Raw>> {
    let parts: Vec<Vec<Raw>> = files
        .par_iter()
        .map(|path| {
            let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
            let mut raws = Vec::new();
            for line in BufReader::new(GzDecoder::new(f)).lines() {
                let line = line.with_context(|| format!("read {}", path.display()))?;
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(feat) = serde_json::from_str::<OaFeature>(&line) else {
                    continue;
                };
                let Some(p) = point(&feat.geometry) else {
                    continue;
                };
                raws.push(Raw {
                    src: Src::OpenAddresses,
                    number: text(&feat.properties.number),
                    street: text(&feat.properties.street),
                    unit: text(&feat.properties.unit),
                    city: text(&feat.properties.city),
                    postcode: text(&feat.properties.postcode),
                    p,
                });
            }
            Ok(raws)
        })
        .collect::<Result<_>>()?;
    Ok(parts.into_iter().flatten().collect())
}

/// Column indexes by header name (a UTF-8 BOM on the first header is ignored).
fn columns<const N: usize>(
    headers: &csv::StringRecord,
    names: [&str; N],
    what: &str,
) -> Result<[usize; N]> {
    let mut out = [0; N];
    for (o, name) in out.iter_mut().zip(names) {
        *o = headers
            .iter()
            .position(|h| h.trim_start_matches('\u{feff}') == name)
            .with_context(|| format!("{what}: no {name} column"))?;
    }
    Ok(out)
}

fn csv_reader<R: Read>(r: R) -> csv::Reader<R> {
    csv::ReaderBuilder::new().flexible(true).from_reader(r)
}

fn zip_entries(zip: &Path) -> Result<Vec<String>> {
    let f = File::open(zip).with_context(|| format!("open {}", zip.display()))?;
    let z = zip::ZipArchive::new(f).with_context(|| format!("read {}", zip.display()))?;
    Ok(z.file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect())
}

/// Opens one zip entry and passes a reader over it to `f`.
fn with_entry<T>(zip: &Path, name: &str, f: impl FnOnce(&mut dyn Read) -> Result<T>) -> Result<T> {
    let file = File::open(zip).with_context(|| format!("open {}", zip.display()))?;
    let mut z = zip::ZipArchive::new(file)?;
    let mut entry = z
        .by_name(name)
        .with_context(|| format!("{name} in {}", zip.display()))?;
    f(&mut entry)
}

fn parse_guid(s: &str) -> Option<u128> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    u128::from_str_radix(&hex, 16).ok()
}

fn lat_lon(lat: &str, lon: &str) -> Option<P> {
    let (lat, lon) = (
        lat.trim().parse::<f64>().ok()?,
        lon.trim().parse::<f64>().ok()?,
    );
    (lat != 0.0 && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
        .then(|| geom::project(lat, lon))
}

/// Street types that come before the name ("Rue X", "Chemin X") rather than after.
const FRENCH_TYPES: &[&str] = &[
    "ALLÉE", "ALLEE", "AUT", "BOUL", "CAR", "CARREF", "CDS", "CERCLE", "CH", "CHASE", "COTE",
    "CÔTE", "CROIS", "IMP", "MONTÉE", "MONTEE", "PASS", "PROM", "RANG", "RG", "RLE", "RTE",
    "ROUTE", "RUE", "RUELLE", "SENT", "TSSE",
];

/// Puts a street name together in the order OSM uses: "du Lac-Wilson" + "CH" gives
/// "CH du Lac-Wilson", "Main" + "ST" + "W" gives "Main ST W". In Quebec the type comes
/// first even for English names ("AV Argyle"), except numbered streets ("118e RUE").
fn compose_street(name: &str, ty: &str, dir: &str, quebec: bool) -> String {
    let (name, ty, dir) = (name.trim(), ty.trim(), dir.trim());
    let numbered = name.starts_with(|c: char| c.is_ascii_digit());
    let type_first = if quebec {
        !numbered
    } else {
        FRENCH_TYPES.contains(&ty.to_uppercase().as_str())
    };
    let parts: Vec<&str> = if type_first {
        vec![ty, name, dir]
    } else {
        vec![name, ty, dir]
    };
    parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn civic(no: &str, suffix: &str) -> String {
    let (no, suffix) = (no.trim(), suffix.trim());
    match suffix.chars().next() {
        None => no.to_owned(),
        Some(c) if c.is_ascii_digit() => format!("{no} {suffix}"),
        Some(_) => format!("{no}{suffix}"),
    }
}

/// A NAR location: its point and whether it is a building point (else blockface).
type Loc = (P, bool);

/// The NAR rows of the provinces with these SGC codes (e.g. "35" Ontario, "24" Quebec).
/// Each address row takes its location's building point, else the blockface point.
pub fn nar(zip: &Path, codes: &[&str]) -> Result<Vec<Raw>> {
    let entries = zip_entries(zip)?;
    let pick = |dir: &str, stem: &str| -> Vec<String> {
        entries
            .iter()
            .filter(|e| {
                codes.iter().any(|c| {
                    let base = format!("{dir}/{stem}_{c}");
                    e.as_str() == format!("{base}.csv") || e.starts_with(&format!("{base}_part_"))
                })
            })
            .cloned()
            .collect()
    };
    let (loc_files, addr_files) = (pick("Locations", "Location"), pick("Addresses", "Address"));
    if loc_files.is_empty() || addr_files.is_empty() {
        bail!("no NAR files for {codes:?} in {}", zip.display());
    }

    // LOC_GUID → (building point, else blockface point; true if building).
    let parts: Vec<Vec<(u128, Option<Loc>)>> = loc_files
        .par_iter()
        .map(|name| {
            with_entry(zip, name, |r| {
                let mut rdr = csv_reader(r);
                let [guid, bg_lat, bg_lon, bf_lat, bf_lon] = columns(
                    rdr.headers()?,
                    [
                        "LOC_GUID",
                        "BG_LATITUDE",
                        "BG_LONGITUDE",
                        "BF_REPPOINT_LATITUDE",
                        "BF_REPPOINT_LONGITUDE",
                    ],
                    name,
                )?;
                let mut out = Vec::new();
                for rec in rdr.records() {
                    let Ok(rec) = rec else { continue };
                    let Some(id) = parse_guid(&rec[guid]) else {
                        continue;
                    };
                    let loc = lat_lon(&rec[bg_lat], &rec[bg_lon])
                        .map(|p| (p, true))
                        .or_else(|| lat_lon(&rec[bf_lat], &rec[bf_lon]).map(|p| (p, false)));
                    out.push((id, loc));
                }
                Ok(out)
            })
        })
        .collect::<Result<_>>()?;
    let mut locs: HashMap<u128, Option<Loc>> = HashMap::new();
    for p in parts {
        locs.extend(p);
    }

    let parts: Vec<Vec<Raw>> = addr_files
        .par_iter()
        .map(|name| {
            with_entry(zip, name, |r| {
                let mut rdr = csv_reader(r);
                let [guid, apt, no, suffix, street, ty, dir, mun, postal] = columns(
                    rdr.headers()?,
                    [
                        "LOC_GUID",
                        "APT_NO_LABEL",
                        "CIVIC_NO",
                        "CIVIC_NO_SUFFIX",
                        "OFFICIAL_STREET_NAME",
                        "OFFICIAL_STREET_TYPE",
                        "OFFICIAL_STREET_DIR",
                        "MAIL_MUN_NAME",
                        "MAIL_POSTAL_CODE",
                    ],
                    name,
                )?;
                let mut raws = Vec::new();
                let quebec = name.contains("_24");
                for rec in rdr.records() {
                    let Ok(rec) = rec else {
                        continue;
                    };
                    let loc =
                        parse_guid(&rec[guid]).and_then(|id| locs.get(&id).copied().flatten());
                    let Some((p, building)) = loc else {
                        continue;
                    };
                    raws.push(Raw {
                        src: if building {
                            Src::Nar
                        } else {
                            Src::NarBlockface
                        },
                        number: civic(&rec[no], &rec[suffix]),
                        street: compose_street(&rec[street], &rec[ty], &rec[dir], quebec),
                        unit: rec[apt].trim().to_owned(),
                        city: rec[mun].trim().to_owned(),
                        postcode: rec[postal].trim().to_owned(),
                        p,
                    });
                }
                Ok(raws)
            })
        })
        .collect::<Result<_>>()?;
    Ok(parts.into_iter().flatten().collect())
}

/// One ODA province zip; keeps rows whose `pruid` is the province's SGC code.
pub fn oda(zip: &Path, abbr: &str, code: &str) -> Result<Vec<Raw>> {
    let name = format!("ODA_{abbr}_v1.csv");
    with_entry(zip, &name, |r| {
        let mut rdr = csv_reader(r);
        let [
            lat,
            lon,
            no,
            street,
            str_name,
            str_type,
            str_dir,
            unit,
            city,
            postal,
            pruid,
        ] = columns(
            rdr.headers()?,
            [
                "latitude",
                "longitude",
                "street_no",
                "street",
                "str_name",
                "str_type",
                "str_dir",
                "unit",
                "city",
                "postal_code",
                "pruid",
            ],
            &name,
        )?;
        let mut raws = Vec::new();
        for rec in rdr.records() {
            let Ok(rec) = rec else {
                continue;
            };
            if rec[pruid].trim() != code {
                continue;
            }
            let Some(p) = lat_lon(&rec[lat], &rec[lon]) else {
                continue;
            };
            let full = rec[street].trim();
            raws.push(Raw {
                src: Src::Oda,
                number: rec[no].trim().to_owned(),
                street: if full.is_empty() {
                    compose_street(&rec[str_name], &rec[str_type], &rec[str_dir], code == "24")
                } else {
                    full.to_owned()
                },
                unit: rec[unit].trim().to_owned(),
                city: rec[city].trim().to_owned(),
                postcode: rec[postal].trim().to_owned(),
                p,
            });
        }
        Ok(raws)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nar_street_and_civic() {
        assert_eq!(
            compose_street("du Lac-Wilson", "CH", "", false),
            "CH du Lac-Wilson"
        );
        assert_eq!(compose_street("Main", "ST", "W", false), "Main ST W");
        assert_eq!(compose_street("Argyle", "AV", "", true), "AV Argyle");
        assert_eq!(compose_street("118e", "RUE", "", true), "118e RUE");
        assert_eq!(civic("123", "A"), "123A");
        assert_eq!(civic("123", "1/2"), "123 1/2");
        assert_eq!(
            parse_guid("b25d5524-b1ea-4cdf-8122-45db85333cc4"),
            Some(0xb25d5524_b1ea_4cdf_8122_45db85333cc4)
        );
    }
}
