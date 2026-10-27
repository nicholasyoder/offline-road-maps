//! Turns a region and a source into a list of download jobs.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use ureq::Agent;

use super::http::{get_json, get_string};
use super::manifest::License;
use super::regions::{self, Region, Subdivision};
use super::{Job, LicenseSpec, Source};

pub fn osm(agent: &Agent, region: &Region, snapshot: &str) -> Result<Vec<Job>> {
    let url = region.pbf_url(snapshot)?;
    // The .md5 file is "<hex>  <filename>".
    let md5 = get_string(agent, &format!("{url}.md5"))?
        .split_whitespace()
        .next()
        .context("empty .md5 file")?
        .to_owned();
    let name = url.rsplit('/').next().unwrap_or_default();
    Ok(vec![Job {
        path: format!("osm/{name}"),
        source: Source::Osm,
        source_id: Some(region.id.clone()),
        url,
        expected_size: None,
        expected_md5: Some(md5),
        license: LicenseSpec::Known(odbl()),
    }])
}

fn odbl() -> License {
    License {
        name: "ODbL-1.0".into(),
        url: Some("https://opendatacommons.org/licenses/odbl/1-0/".into()),
        attribution_required: Some(true),
        attribution: Some("© OpenStreetMap contributors".into()),
        share_alike: Some(true),
    }
}

/// Ocean polygons derived from OSM coastlines, split into small pieces, in WGS84. Global, and
/// only the latest build is published, so it cannot be pinned to the OSM snapshot date.
pub fn water() -> Vec<Job> {
    let name = "water-polygons-split-4326.zip";
    vec![Job {
        path: format!("osmdata/{name}"),
        source: Source::Water,
        source_id: None,
        url: format!("https://osmdata.openstreetmap.de/download/{name}"),
        expected_size: None,
        expected_md5: None,
        license: LicenseSpec::Known(odbl()),
    }]
}

/// Boundary polygons (`.poly`) of Geofabrik extracts: the region's own, saved as
/// `poly/<region id>.poly` (for `--clip-poly`), and each subdivision's, saved as
/// `poly/<ISO code>.poly` (for `build --addresses`). Geofabrik publishes only the current
/// polygons, so they are not date-pinned.
pub fn polys(index_json: &str, region: &Region) -> Result<Vec<Job>> {
    let job = |r: &Region, path: String| -> Result<Job> {
        let base = r
            .latest_pbf
            .strip_suffix("-latest.osm.pbf")
            .with_context(|| format!("unexpected Geofabrik URL {}", r.latest_pbf))?;
        Ok(Job {
            path,
            source: Source::Poly,
            source_id: Some(r.id.clone()),
            url: format!("{base}.poly"),
            expected_size: None,
            expected_md5: None,
            license: LicenseSpec::Known(odbl()),
        })
    };
    let mut jobs = vec![job(region, format!("poly/{}.poly", region.id))?];
    for &sub in &region.subdivisions {
        let id = regions::subdivision_extract(index_json, sub)?;
        let r = regions::resolve(index_json, &id)?;
        jobs.push(job(&r, format!("poly/{}.poly", sub.iso()))?);
    }
    Ok(jobs)
}

/// TIGER/Line `ADDRFEAT`: one zip per county, listed from the Census directory index.
pub fn tiger(agent: &Agent, region: &Region, year: u16) -> Result<Vec<Job>> {
    let states: Vec<&str> = region
        .subdivisions
        .iter()
        .filter_map(|s| match s {
            Subdivision::UsState { fips, .. } => Some(*fips),
            Subdivision::CaProvince { .. } => None,
        })
        .collect();
    if states.is_empty() {
        return Ok(Vec::new());
    }
    let base = format!("https://www2.census.gov/geo/tiger/TIGER{year}/ADDRFEAT/");
    let listing = get_string(agent, &base)?;
    let license = License {
        name: "Public domain (US Government work)".into(),
        url: Some("https://www.census.gov/data/developers/about/terms-of-service.html".into()),
        attribution_required: Some(false),
        attribution: Some(format!("U.S. Census Bureau, TIGER/Line Shapefiles {year}")),
        share_alike: Some(false),
    };
    let jobs: Vec<Job> = parse_addrfeat_listing(&listing, year)
        .into_iter()
        .filter(|(state, _)| states.contains(&state.as_str()))
        .map(|(_, file)| Job {
            path: format!("tiger/{year}/{file}"),
            source: Source::Tiger,
            source_id: None,
            url: format!("{base}{file}"),
            expected_size: None,
            expected_md5: None,
            license: LicenseSpec::Known(license.clone()),
        })
        .collect();
    anyhow::ensure!(
        !jobs.is_empty(),
        "no ADDRFEAT files for {states:?} in {base}"
    );
    Ok(jobs)
}

/// Finds `tl_<year>_<SSCCC>_addrfeat.zip` names; returns `(state FIPS, file name)`.
fn parse_addrfeat_listing(html: &str, year: u16) -> BTreeSet<(String, String)> {
    let prefix = format!("tl_{year}_");
    let mut found = BTreeSet::new();
    let mut rest = html;
    while let Some(i) = rest.find(&prefix) {
        rest = &rest[i + prefix.len()..];
        let Some(geoid) = rest.get(..5) else { break };
        if geoid.bytes().all(|b| b.is_ascii_digit()) && rest[5..].starts_with("_addrfeat.zip") {
            found.insert((
                geoid[..2].to_owned(),
                format!("{prefix}{geoid}_addrfeat.zip"),
            ));
        }
    }
    found
}

const OA_API: &str = "https://batch.openaddresses.io/api";

#[derive(Deserialize)]
struct OaData {
    source: String,
    name: String,
    job: Option<u64>,
    size: Option<u64>,
}

/// OpenAddresses: the latest successful `addresses` job of every source in the region.
/// Listing and download need no account; licenses are looked up per job at download time.
pub fn openaddresses(agent: &Agent, region: &Region) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for sub in &region.subdivisions {
        let prefix = sub.oa_prefix();
        let data: Vec<OaData> = get_json(
            agent,
            &format!("{OA_API}/data?source={prefix}&layer=addresses"),
        )?;
        for d in data {
            // The API matches by prefix, so `us/o` would also return `us/oh/...`.
            if !d.source.starts_with(&format!("{prefix}/")) {
                continue;
            }
            let (Some(job), Some(size)) = (d.job, d.size) else {
                eprintln!(
                    "  openaddresses: {}/{} has no output, skipped",
                    d.source, d.name
                );
                continue;
            };
            jobs.push(Job {
                path: format!("openaddresses/{}/{}.geojson.gz", d.source, d.name),
                source: Source::OpenAddresses,
                source_id: Some(format!("{}/{} (job {job})", d.source, d.name)),
                url: format!("https://v2.openaddresses.io/batch-prod/job/{job}/source.geojson.gz"),
                expected_size: Some(size),
                expected_md5: None,
                license: LicenseSpec::OpenAddresses { job, layer: d.name },
            });
        }
    }
    Ok(jobs)
}

#[derive(Deserialize)]
struct OaJob {
    /// The source definition at the commit the job ran from.
    source: String,
}

/// Returns the license and the source-definition URL it was read from.
pub fn openaddresses_license(agent: &Agent, job: u64, layer: &str) -> Result<(License, String)> {
    let meta: OaJob = get_json(agent, &format!("{OA_API}/job/{job}"))?;
    let def: Value = match get_json(agent, &meta.source) {
        Ok(def) => def,
        // Some jobs point at a source definition on `master` that has since been deleted.
        Err(e) if matches!(e.downcast_ref(), Some(ureq::Error::StatusCode(404))) => {
            let mut l = oa_license(&Value::Null, layer);
            l.name = "unspecified (source definition deleted upstream)".into();
            return Ok((l, meta.source));
        }
        Err(e) => return Err(e),
    };
    Ok((oa_license(&def, layer), meta.source))
}

fn oa_license(def: &Value, layer: &str) -> License {
    let layers = def["layers"]["addresses"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let layer = layers
        .iter()
        .find(|l| l["name"] == layer)
        .or(layers.first().filter(|_| layers.len() == 1));
    let lic = layer
        .and_then(|l| l.get("license"))
        .or_else(|| def.get("license"));
    let str_field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    match lic {
        Some(Value::String(s)) => License {
            name: s.clone(),
            url: None,
            attribution_required: None,
            attribution: None,
            share_alike: None,
        },
        Some(obj @ Value::Object(_)) => License {
            name: str_field(obj, "text")
                .or_else(|| str_field(obj, "url"))
                .unwrap_or_else(|| "unspecified".into()),
            url: str_field(obj, "url"),
            attribution_required: obj.get("attribution").and_then(Value::as_bool),
            attribution: str_field(obj, "attribution name"),
            share_alike: obj.get("share-alike").and_then(Value::as_bool),
        },
        _ => License {
            name: "unspecified".into(),
            url: None,
            attribution_required: None,
            attribution: None,
            share_alike: None,
        },
    }
}

const STATCAN_PUB: &str = "https://www150.statcan.gc.ca/n1/pub";

/// Provinces and territories with an ODA v1 file (there is none for NL, NU and YT).
const ODA_PROVINCES: &[&str] = &["AB", "BC", "MB", "NB", "NS", "NT", "ON", "PE", "QC", "SK"];

/// Statistics Canada Open Database of Addresses (2021), one zip per province.
pub fn oda(region: &Region) -> Vec<Job> {
    let license = License {
        name: "Open Government Licence – Canada".into(),
        url: Some("https://open.canada.ca/en/open-government-licence-canada".into()),
        attribution_required: Some(true),
        attribution: Some(
            "Contains information licensed under the Open Government Licence – Canada.".into(),
        ),
        share_alike: Some(false),
    };
    let mut jobs = Vec::new();
    for sub in &region.subdivisions {
        let Subdivision::CaProvince { abbr } = *sub else {
            continue;
        };
        if !ODA_PROVINCES.contains(&abbr) {
            eprintln!("  oda: no file for {abbr}, skipped");
            continue;
        }
        let file = format!("ODA_{abbr}_v1.zip");
        jobs.push(Job {
            path: format!("statcan/oda/{file}"),
            source: Source::Oda,
            source_id: Some(abbr.to_owned()),
            url: format!("{STATCAN_PUB}/46-26-0001/2021001/{file}"),
            expected_size: None,
            expected_md5: None,
            license: LicenseSpec::Known(license.clone()),
        });
    }
    jobs
}

/// Statistics Canada National Address Register: one national zip per release (`YYYYMM`).
pub fn nar(region: &Region, release: &str) -> Vec<Job> {
    if !region
        .subdivisions
        .iter()
        .any(|s| matches!(s, Subdivision::CaProvince { .. }))
    {
        return Vec::new();
    }
    vec![Job {
        path: format!("statcan/nar/{release}.zip"),
        source: Source::Nar,
        source_id: Some(release.to_owned()),
        url: format!("{STATCAN_PUB}/46-26-0002/2022001/{release}.zip"),
        expected_size: None,
        expected_md5: None,
        license: LicenseSpec::Known(License {
            name: "Statistics Canada Open Licence".into(),
            url: Some("https://www.statcan.gc.ca/en/terms-conditions/open-licence".into()),
            attribution_required: Some(true),
            attribution: Some(format!(
                "Adapted from Statistics Canada, National Address Register, {release}. \
                 This does not constitute an endorsement by Statistics Canada of this product. \
                 Contains information licensed under the Open Government Licence – Yukon."
            )),
            share_alike: Some(false),
        }),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addrfeat_listing() {
        let html = r#"<a href="tl_2026_39001_addrfeat.zip">tl_2026_39001_addrfeat.zip</a>
            <a href="tl_2026_39003_addrfeat.zip">x</a> <a href="tl_2026_01001_addrfeat.zip">
            <a href="tl_2025_39005_addrfeat.zip"> <a href="tl_2026_39x07_addrfeat.zip">"#;
        let found: Vec<_> = parse_addrfeat_listing(html, 2026).into_iter().collect();
        assert_eq!(
            found,
            [
                ("01".into(), "tl_2026_01001_addrfeat.zip".into()),
                ("39".into(), "tl_2026_39001_addrfeat.zip".into()),
                ("39".into(), "tl_2026_39003_addrfeat.zip".into()),
            ]
        );
    }

    #[test]
    fn oa_license_object_and_missing() {
        let def: Value = serde_json::from_str(
            r#"{"layers": {"addresses": [
                {"name": "city", "license": {"text": "CC BY 4.0", "url": "https://cc/by",
                 "attribution": true, "attribution name": "City of X", "share-alike": false}},
                {"name": "county"}
            ]}}"#,
        )
        .unwrap();
        let city = oa_license(&def, "city");
        assert_eq!(city.name, "CC BY 4.0");
        assert_eq!(city.attribution.as_deref(), Some("City of X"));
        assert_eq!(city.attribution_required, Some(true));
        assert_eq!(city.share_alike, Some(false));
        assert_eq!(oa_license(&def, "county").name, "unspecified");
    }

    #[test]
    fn oa_license_string_at_top_level() {
        let def: Value = serde_json::from_str(
            r#"{"license": "Public Domain", "layers": {"addresses": [{"name": "county"}]}}"#,
        )
        .unwrap();
        assert_eq!(oa_license(&def, "county").name, "Public Domain");
    }
}
