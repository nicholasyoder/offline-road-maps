//! `META`: what the pack covers, where its data came from, and the attribution to show.

use std::io;

use serde::{Deserialize, Serialize};

use crate::zframe;

pub const ID: [u8; 4] = *b"META";

/// Fields may be added in later minor versions; unknown ones are ignored when reading.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub region: String,
    /// West, south, east, north in degrees.
    pub bbox: [f64; 4],
    /// Unix seconds, UTC.
    pub build_time: i64,
    pub builder_version: String,
    /// `major.minor` of the container, e.g. `0.1`.
    pub format_version: String,
    #[serde(default)]
    pub sources: Vec<Source>,
    /// The text for the app's "About this map" screen.
    #[serde(default)]
    pub attribution: String,
    /// The build options that shaped the data (whitelist version, tolerances, …).
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// One input file, from the `fetch` manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// E.g. `osm`, or `openaddresses us/oh/cuyahoga`.
    pub name: String,
    pub url: String,
    /// When it was downloaded, RFC 3339.
    pub date: String,
    pub sha256: String,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
}

impl Meta {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        zframe::to_json_section(self)
    }

    pub fn decode(b: &[u8]) -> io::Result<Self> {
        zframe::from_json_section(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_extra_fields() {
        let mut m = Meta {
            region: "Ohio".into(),
            bbox: [-84.82, 38.4, -80.51, 41.98],
            build_time: 1_793_000_000,
            builder_version: "0.1.0".into(),
            format_version: "0.1".into(),
            sources: vec![Source {
                name: "osm".into(),
                url: "https://download.geofabrik.de/north-america/us/ohio-261001.osm.pbf".into(),
                date: "2026-10-02T10:00:00Z".into(),
                sha256: "ab".repeat(32),
                license: "ODbL-1.0".into(),
                license_url: None,
                attribution: Some("© OpenStreetMap contributors".into()),
            }],
            attribution: "© OpenStreetMap contributors, ODbL".into(),
            settings: Default::default(),
        };
        m.settings.insert("whitelist_version".into(), 3.into());
        assert_eq!(Meta::decode(&m.encode().unwrap()).unwrap(), m);

        // A newer builder's extra field is ignored.
        let json = br#"{"region":"X","bbox":[0,0,1,1],"build_time":0,"builder_version":"9",
            "format_version":"0.2","later":true}"#;
        let frame = zstd::bulk::compress(json, 3).unwrap();
        assert_eq!(Meta::decode(&frame).unwrap().region, "X");
    }
}
