//! Resolves a Geofabrik region id (`us/ohio`, `ontario`, `north-america`, ...) to its OSM
//! extract and the US states and Canadian provinces it covers.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const GEOFABRIK_INDEX: &str = "https://download.geofabrik.de/index-v1-nogeom.json";

#[derive(Debug)]
pub struct Region {
    pub id: String,
    /// The `-latest` extract URL; see [`Region::pbf_url`] for a pinned snapshot.
    pub latest_pbf: String,
    pub subdivisions: Vec<Subdivision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subdivision {
    UsState {
        abbr: &'static str,
        fips: &'static str,
    },
    CaProvince {
        abbr: &'static str,
    },
}

impl Subdivision {
    /// OpenAddresses source prefix, e.g. `us/oh` or `ca/on`.
    pub fn oa_prefix(self) -> String {
        match self {
            Self::UsState { abbr, .. } => format!("us/{}", abbr.to_lowercase()),
            Self::CaProvince { abbr } => format!("ca/{}", abbr.to_lowercase()),
        }
    }

    /// Every US state (with DC, Puerto Rico and the US Virgin Islands) and Canadian province
    /// and territory.
    pub fn all() -> Vec<Self> {
        let us = US_STATE_FIPS
            .iter()
            .map(|&(abbr, fips)| Self::UsState { abbr, fips });
        let ca = CA_PROVINCES.iter().map(|&abbr| Self::CaProvince { abbr });
        us.chain(ca).collect()
    }

    /// ISO 3166-2 code, e.g. `US-OH`.
    pub fn iso(self) -> String {
        match self {
            Self::UsState { abbr, .. } => format!("US-{abbr}"),
            Self::CaProvince { abbr } => format!("CA-{abbr}"),
        }
    }

    /// Parses `--addresses`-style codes (`US-OH`, `ca-qc`, `all`), each subdivision kept
    /// once, where it first appears.
    pub fn parse_list(codes: &[String]) -> Result<Vec<Self>> {
        let mut subs: Vec<Self> = Vec::new();
        for c in codes {
            let found = if c.eq_ignore_ascii_case("all") {
                Self::all()
            } else {
                vec![Self::from_iso(&c.to_uppercase()).with_context(|| {
                    format!("unknown subdivision {c:?}; use e.g. US-OH or CA-QC")
                })?]
            };
            for s in found {
                if !subs.contains(&s) {
                    subs.push(s);
                }
            }
        }
        Ok(subs)
    }

    pub fn from_iso(code: &str) -> Option<Self> {
        if let Some(abbr) = code.strip_prefix("US-") {
            US_STATE_FIPS
                .iter()
                .find(|(a, _)| *a == abbr)
                .map(|&(abbr, fips)| Self::UsState { abbr, fips })
        } else if let Some(abbr) = code.strip_prefix("CA-") {
            CA_PROVINCES
                .iter()
                .find(|a| **a == abbr)
                .map(|&abbr| Self::CaProvince { abbr })
        } else {
            None
        }
    }
}

impl Region {
    /// Geofabrik keeps dated extracts (`ohio-261001.osm.pbf`): daily ones for about a week,
    /// first-of-month ones for several months.
    pub fn pbf_url(&self, snapshot: &str) -> Result<String> {
        match self.latest_pbf.strip_suffix("-latest.osm.pbf") {
            Some(base) => Ok(format!("{base}-{snapshot}.osm.pbf")),
            None => bail!("unexpected Geofabrik URL {}", self.latest_pbf),
        }
    }
}

/// State FIPS codes, used to pick TIGER county files. Geofabrik's North America extract
/// covers the states, DC, Puerto Rico and the US Virgin Islands.
#[rustfmt::skip]
const US_STATE_FIPS: &[(&str, &str)] = &[
    ("AL", "01"), ("AK", "02"), ("AZ", "04"), ("AR", "05"), ("CA", "06"), ("CO", "08"),
    ("CT", "09"), ("DE", "10"), ("DC", "11"), ("FL", "12"), ("GA", "13"), ("HI", "15"),
    ("ID", "16"), ("IL", "17"), ("IN", "18"), ("IA", "19"), ("KS", "20"), ("KY", "21"),
    ("LA", "22"), ("ME", "23"), ("MD", "24"), ("MA", "25"), ("MI", "26"), ("MN", "27"),
    ("MS", "28"), ("MO", "29"), ("MT", "30"), ("NE", "31"), ("NV", "32"), ("NH", "33"),
    ("NJ", "34"), ("NM", "35"), ("NY", "36"), ("NC", "37"), ("ND", "38"), ("OH", "39"),
    ("OK", "40"), ("OR", "41"), ("PA", "42"), ("RI", "44"), ("SC", "45"), ("SD", "46"),
    ("TN", "47"), ("TX", "48"), ("UT", "49"), ("VT", "50"), ("VA", "51"), ("WA", "53"),
    ("WV", "54"), ("WI", "55"), ("WY", "56"), ("PR", "72"), ("VI", "78"),
];

const CA_PROVINCES: &[&str] = &[
    "AB", "BC", "MB", "NB", "NL", "NS", "NT", "NU", "ON", "PE", "QC", "SK", "YT",
];

#[derive(Deserialize)]
struct Index {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    properties: Props,
}

#[derive(Deserialize)]
struct Props {
    id: String,
    parent: Option<String>,
    #[serde(rename = "iso3166-1:alpha2", default)]
    iso_country: Vec<String>,
    #[serde(rename = "iso3166-2", default)]
    iso_subdivision: Vec<String>,
    urls: Urls,
}

#[derive(Deserialize)]
struct Urls {
    pbf: String,
}

/// The Geofabrik extract that covers exactly one subdivision, e.g. `us/ohio` for `US-OH`.
pub fn subdivision_extract(index_json: &str, sub: Subdivision) -> Result<String> {
    let index: Index = serde_json::from_str(index_json).context("parse Geofabrik index")?;
    let iso = sub.iso();
    index
        .features
        .into_iter()
        .map(|f| f.properties)
        .find(|p| p.iso_subdivision == [iso.as_str()])
        .map(|p| p.id)
        .with_context(|| format!("no Geofabrik extract for {iso}"))
}

pub fn resolve(index_json: &str, id: &str) -> Result<Region> {
    let index: Index = serde_json::from_str(index_json).context("parse Geofabrik index")?;
    let props: Vec<&Props> = index.features.iter().map(|f| &f.properties).collect();
    let Some(target) = props.iter().find(|p| p.id == id) else {
        bail!("unknown Geofabrik region {id:?}; ids look like us/ohio, ontario, north-america");
    };

    // A region's subdivisions are its own ISO 3166-2 codes, else those of every descendant,
    // else (for `us` and `canada`, whose states hang off `north-america`) every code in
    // its country.
    let is_descendant = |p: &Props| {
        let mut parent = p.parent.as_deref();
        while let Some(pid) = parent {
            if pid == id {
                return true;
            }
            parent = props
                .iter()
                .find(|q| q.id == pid)
                .and_then(|q| q.parent.as_deref());
        }
        false
    };
    let mut codes: Vec<&str> = target.iso_subdivision.iter().map(String::as_str).collect();
    if codes.is_empty() {
        codes = props
            .iter()
            .filter(|p| is_descendant(p))
            .flat_map(|p| p.iso_subdivision.iter().map(String::as_str))
            .collect();
    }
    if codes.is_empty() {
        codes = props
            .iter()
            .flat_map(|p| p.iso_subdivision.iter().map(String::as_str))
            .filter(|c| {
                target.iso_country.iter().any(|country| {
                    c.strip_prefix(country.as_str())
                        .is_some_and(|r| r.starts_with('-'))
                })
            })
            .collect();
    }

    let mut subdivisions: Vec<Subdivision> = codes
        .into_iter()
        .filter_map(Subdivision::from_iso)
        .collect();
    subdivisions.sort_by_key(|s| s.oa_prefix());
    subdivisions.dedup();
    Ok(Region {
        id: id.to_owned(),
        latest_pbf: target.urls.pbf.clone(),
        subdivisions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{"features": [
        {"properties": {"id": "north-america", "urls": {"pbf": "https://d/north-america-latest.osm.pbf"}}},
        {"properties": {"id": "us", "parent": "north-america", "iso3166-1:alpha2": ["US"], "urls": {"pbf": "https://d/north-america/us-latest.osm.pbf"}}},
        {"properties": {"id": "canada", "parent": "north-america", "iso3166-1:alpha2": ["CA"], "urls": {"pbf": "https://d/north-america/canada-latest.osm.pbf"}}},
        {"properties": {"id": "mexico", "parent": "north-america", "urls": {"pbf": "https://d/north-america/mexico-latest.osm.pbf"}}},
        {"properties": {"id": "us/ohio", "parent": "north-america", "iso3166-2": ["US-OH"], "urls": {"pbf": "https://d/north-america/us/ohio-latest.osm.pbf"}}},
        {"properties": {"id": "us/delaware", "parent": "north-america", "iso3166-2": ["US-DE"], "urls": {"pbf": "https://d/north-america/us/delaware-latest.osm.pbf"}}},
        {"properties": {"id": "ontario", "parent": "canada", "iso3166-2": ["CA-ON"], "urls": {"pbf": "https://d/north-america/canada/ontario-latest.osm.pbf"}}}
    ]}"#;

    #[test]
    fn subdivision_lists_keep_each_once() {
        let codes = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let isos = |c: &[&str]| -> Vec<String> {
            let subs = Subdivision::parse_list(&codes(c)).unwrap();
            subs.into_iter().map(Subdivision::iso).collect()
        };
        let all: Vec<String> = Subdivision::all()
            .into_iter()
            .map(Subdivision::iso)
            .collect();
        assert_eq!(isos(&["all"]), all);
        assert_eq!(isos(&["all", "us-oh"]), all);
        let oh_all = isos(&["US-OH", "all"]);
        assert_eq!(oh_all.len(), all.len());
        assert_eq!(oh_all.iter().filter(|s| *s == "US-OH").count(), 1);
        assert_eq!(isos(&["CA-QC", "US-OH", "CA-QC"]), ["CA-QC", "US-OH"]);
        assert!(Subdivision::parse_list(&codes(&["US-XX"])).is_err());
    }

    #[test]
    fn single_state() {
        let r = resolve(INDEX, "us/ohio").unwrap();
        assert_eq!(
            r.subdivisions,
            [Subdivision::UsState {
                abbr: "OH",
                fips: "39"
            }]
        );
        assert_eq!(
            r.pbf_url("261001").unwrap(),
            "https://d/north-america/us/ohio-261001.osm.pbf"
        );
    }

    #[test]
    fn continent_and_countries() {
        let all: Vec<_> = resolve(INDEX, "north-america")
            .unwrap()
            .subdivisions
            .iter()
            .map(|s| s.oa_prefix())
            .collect();
        assert_eq!(all, ["ca/on", "us/de", "us/oh"]);
        assert_eq!(resolve(INDEX, "us").unwrap().subdivisions.len(), 2);
        assert_eq!(
            resolve(INDEX, "canada").unwrap().subdivisions,
            [Subdivision::CaProvince { abbr: "ON" }]
        );
        assert!(resolve(INDEX, "mexico").unwrap().subdivisions.is_empty());
        assert!(resolve(INDEX, "ohio").is_err());
    }

    #[test]
    fn subdivision_extracts() {
        let on = Subdivision::from_iso("CA-ON").unwrap();
        assert_eq!(on.iso(), "CA-ON");
        assert_eq!(subdivision_extract(INDEX, on).unwrap(), "ontario");
        let qc = Subdivision::from_iso("CA-QC").unwrap();
        assert!(subdivision_extract(INDEX, qc).is_err());
    }
}
