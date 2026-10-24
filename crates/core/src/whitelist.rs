//! The tag whitelist: maps OSM tags to pack layers and classes. See `data/whitelist.toml`.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The whitelist compiled into this build.
pub const DEFAULT: &str = include_str!("../data/whitelist.toml");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Whitelist {
    pub version: u32,
    #[serde(rename = "layer")]
    pub layers: Vec<Layer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    pub geometry: GeometryKind,
    pub from: Vec<ElementType>,
    pub props: Vec<String>,
    pub classes: Vec<Class>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Class {
    pub name: String,
    #[serde(default)]
    pub tags: BTreeMap<String, Values>,
    #[serde(default)]
    pub absent: Vec<String>,
    pub minzoom: u8,
    #[serde(default = "yes")]
    pub tile: bool,
    pub geometry: Option<GeometryKind>,
    #[serde(default)]
    pub external: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Values {
    One(String),
    Any(Vec<String>),
}

impl Values {
    fn matches(&self, v: &str) -> bool {
        match self {
            Values::One(s) => s == "*" || s == v,
            Values::Any(list) => list.iter().any(|s| s == "*" || s == v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeometryKind {
    Point,
    Line,
    Area,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementType {
    Node,
    Way,
    Relation,
}

impl Whitelist {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn layer_index(&self, name: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.name == name)
    }

    /// Every tag key any class looks at, for skipping untagged or irrelevant elements quickly.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self
            .layers
            .iter()
            .flat_map(|l| &l.classes)
            .flat_map(|c| c.tags.keys().map(String::as_str))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

impl Layer {
    /// The first class whose conditions hold for `tags`.
    pub fn classify(&self, tags: &[(&str, &str)]) -> Option<usize> {
        self.classes.iter().position(|c| c.matches(tags))
    }

    pub fn geometry_of(&self, class: usize) -> GeometryKind {
        self.classes[class].geometry.unwrap_or(self.geometry)
    }
}

impl Class {
    pub fn matches(&self, tags: &[(&str, &str)]) -> bool {
        let get = |k: &str| tags.iter().find(|(tk, _)| *tk == k).map(|(_, v)| *v);
        !self.external
            && !self.tags.is_empty()
            && self
                .tags
                .iter()
                .all(|(k, vals)| get(k).is_some_and(|v| vals.matches(v)))
            && self.absent.iter().all(|k| get(k).is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_parses_and_classifies() {
        let wl = Whitelist::parse(DEFAULT).unwrap();
        let roads = &wl.layers[wl.layer_index("roads").unwrap()];
        let class =
            |tags: &[(&str, &str)]| roads.classify(tags).map(|i| &roads.classes[i].name[..]);
        assert_eq!(class(&[("highway", "service")]), Some("service"));
        assert_eq!(
            class(&[("highway", "service"), ("service", "driveway")]),
            Some("service_driveway")
        );
        assert_eq!(
            class(&[("highway", "service"), ("service", "emergency_access")]),
            Some("service_other")
        );
        assert_eq!(class(&[("highway", "footway")]), None);

        let water = &wl.layers[wl.layer_index("water").unwrap()];
        assert_eq!(water.classify(&[("natural", "water")]), Some(1));
        assert_eq!(water.geometry_of(3), GeometryKind::Line);
        // The external ocean class never matches OSM tags.
        assert_eq!(water.classify(&[]), None);
        assert!(wl.keys().contains(&"addr:housenumber"));
    }
}
