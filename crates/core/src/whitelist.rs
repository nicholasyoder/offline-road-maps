//! The tag whitelist: maps OSM tags to pack layers and classes. See `data/whitelist.toml`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    /// Douglas–Peucker tolerance per zoom (`z0`..`z14`), in tile grid units; zooms not
    /// listed use the builder's default.
    #[serde(default)]
    pub tolerance: BTreeMap<String, f64>,
    /// Join line pieces of the same class and props end to end before encoding (decision 5).
    #[serde(default)]
    pub merge_lines: Option<MergeLines>,
    /// The last zoom whose tiles hold this layer; above it the app draws the layer from the
    /// tile at this zoom, scaled up (format 0.3). None: every zoom up to z14.
    #[serde(default)]
    pub maxzoom: Option<u8>,
    pub classes: Vec<Class>,
}

/// How [`Layer::merge_lines`] may join two pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeLines {
    /// A piece may be reversed to join, unless it has a `oneway` prop other than `no`.
    Reversible,
    /// Pieces join only end to start, so their direction is kept (waterways flow).
    Directed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Class {
    pub name: String,
    #[serde(default)]
    pub tags: BTreeMap<String, Values>,
    #[serde(default)]
    pub absent: Vec<String>,
    /// Tags that rule the class out when present with one of the listed values.
    #[serde(default)]
    pub unless: BTreeMap<String, Values>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        let wl: Self = toml::from_str(text)?;
        for l in &wl.layers {
            for (k, v) in &l.tolerance {
                let zoom = k.strip_prefix('z').and_then(|z| z.parse::<u8>().ok());
                if !zoom.is_some_and(|z| z <= 14) || !(v.is_finite() && *v >= 0.0) {
                    return Err(serde::de::Error::custom(format!(
                        "layer {}: bad tolerance {k} = {v} (keys are z0..z14, values >= 0)",
                        l.name
                    )));
                }
            }
            if let Some(m) = l.maxzoom {
                let first = l.classes.iter().filter(|c| c.tile).map(|c| c.minzoom).max();
                if m > 14 || first.is_some_and(|f| f > m) {
                    return Err(serde::de::Error::custom(format!(
                        "layer {}: maxzoom {m} must be at most 14 and not below any class's minzoom",
                        l.name
                    )));
                }
            }
        }
        Ok(wl)
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
    /// The Douglas–Peucker tolerance this layer sets for zoom `z`, if any.
    pub fn tolerance(&self, z: u8) -> Option<f64> {
        self.tolerance.get(&format!("z{z}")).copied()
    }

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
            && !self
                .unless
                .iter()
                .any(|(k, vals)| get(k).is_some_and(|v| vals.matches(v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_tolerance() {
        let layer = |tol: &str| {
            format!(
                "version = 1\n[[layer]]\nname = \"water\"\ngeometry = \"area\"\nfrom = [\"way\"]\n\
                 props = []\ntolerance = {tol}\nclasses = [{{ name = \"lake\", minzoom = 6 }}]\n"
            )
        };
        let wl = Whitelist::parse(&layer("{ z13 = 6.0, z14 = 4 }")).unwrap();
        assert_eq!(wl.layers[0].tolerance(14), Some(4.0));
        assert_eq!(wl.layers[0].tolerance(12), None);
        assert!(Whitelist::parse(&layer("{ z15 = 1.0 }")).is_err());
        assert!(Whitelist::parse(&layer("{ z14 = -1.0 }")).is_err());
        let capped = |m: u8| layer("{}").replace("classes", &format!("maxzoom = {m}\nclasses"));
        assert_eq!(
            Whitelist::parse(&capped(13)).unwrap().layers[0].maxzoom,
            Some(13)
        );
        assert!(Whitelist::parse(&capped(5)).is_err());
        assert!(Whitelist::parse(&capped(15)).is_err());
        assert_eq!(
            Whitelist::parse(DEFAULT).unwrap().layers[0].tolerance(14),
            None
        );
    }

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
        assert_eq!(water.geometry_of(1), GeometryKind::Area);
        assert_eq!(water.maxzoom, Some(13));
        let ways = &wl.layers[wl.layer_index("waterways").unwrap()];
        assert_eq!(ways.classify(&[("waterway", "river")]), Some(0));
        // A river area tagged as water is a lake in `water`, not also a line.
        assert_eq!(
            ways.classify(&[("waterway", "canal"), ("natural", "water")]),
            None
        );
        assert_eq!(ways.geometry_of(0), GeometryKind::Line);
        assert_eq!(ways.maxzoom, None);
        // The external ocean class never matches OSM tags.
        assert_eq!(water.classify(&[]), None);
        assert!(wl.keys().contains(&"addr:housenumber"));
    }
}
