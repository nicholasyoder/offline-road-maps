//! `SCHM`: the layer, class and prop IDs that tiles refer to, so the app can resolve the
//! style's names without a compiled-in whitelist.

use std::io;

use serde::{Deserialize, Serialize};

use crate::whitelist::{GeometryKind, Whitelist};
use crate::zframe;

pub const ID: [u8; 4] = *b"SCHM";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub whitelist_version: u32,
    /// In layer-ID order.
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub geometry: GeometryKind,
    /// In prop-key order.
    pub props: Vec<String>,
    /// The last zoom whose tiles hold this layer; above it, draw it from the tile at this
    /// zoom (format 0.3). None: every zoom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maxzoom: Option<u8>,
    /// In class-ID order.
    pub classes: Vec<Class>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Class {
    pub name: String,
    pub minzoom: u8,
}

impl Schema {
    /// IDs are positions in the whitelist, as the builder assigns them.
    pub fn from_whitelist(wl: &Whitelist) -> Self {
        Self {
            whitelist_version: wl.version,
            layers: wl
                .layers
                .iter()
                .map(|l| Layer {
                    name: l.name.clone(),
                    geometry: l.geometry,
                    props: l.props.clone(),
                    maxzoom: l.maxzoom,
                    classes: l
                        .classes
                        .iter()
                        .map(|c| Class {
                            name: c.name.clone(),
                            minzoom: c.minzoom,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        zframe::to_json_section(self)
    }

    pub fn decode(b: &[u8]) -> io::Result<Self> {
        zframe::from_json_section(b)
    }

    pub fn layer(&self, name: &str) -> Option<(u8, &Layer)> {
        let i = self.layers.iter().position(|l| l.name == name)?;
        Some((i as u8, &self.layers[i]))
    }
}

impl Layer {
    pub fn class(&self, name: &str) -> Option<u16> {
        self.classes
            .iter()
            .position(|c| c.name == name)
            .map(|i| i as u16)
    }

    pub fn prop(&self, name: &str) -> Option<u8> {
        self.props.iter().position(|p| p == name).map(|i| i as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_the_default_whitelist() {
        let wl = Whitelist::parse(crate::whitelist::DEFAULT).unwrap();
        let s = Schema::from_whitelist(&wl);
        let back = Schema::decode(&s.encode().unwrap()).unwrap();
        assert_eq!(back, s);
        let (id, roads) = back.layer("roads").unwrap();
        assert_eq!(usize::from(id), wl.layer_index("roads").unwrap());
        assert_eq!(roads.geometry, GeometryKind::Line);
        assert!(roads.class("motorway").is_some() && roads.prop("routes").is_some());
        assert!(back.layer("nope").is_none());
    }
}
