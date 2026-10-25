//! Layer toggles (phase1-plan.md 1.9): land cover, boundaries, rail, labels, shields and
//! four groups of POIs. A toggle switches style rules off at draw time, so nothing is read
//! or tessellated again: hidden paints are sent to the GPU as transparent with no width,
//! like a paint outside its zoom range, and hidden labels are not placement candidates.

use omp_core::schema::Schema;
use serde::{Deserialize, Serialize};

use crate::style::Style;

/// What is shown; saved in `config.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layers {
    pub landcover: bool,
    pub boundaries: bool,
    pub rail: bool,
    /// Place, area, road and water names (POI names go with their POIs).
    pub labels: bool,
    pub shields: bool,
    pub fuel: bool,
    pub lodging: bool,
    pub food: bool,
    pub other_pois: bool,
}

impl Default for Layers {
    fn default() -> Self {
        Self {
            landcover: true,
            boundaries: true,
            rail: true,
            labels: true,
            shields: true,
            fuel: true,
            lodging: true,
            food: true,
            other_pois: true,
        }
    }
}

/// POI classes by group; classes in no group are "other".
const FUEL: &[&str] = &["fuel", "charging_station"];
const LODGING: &[&str] = &["hotel", "campground"];
const FOOD: &[&str] = &["restaurant", "fast_food"];

impl Layers {
    /// The toggles with their names in the Layers menu, in menu order.
    pub fn entries(&mut self) -> [(&'static str, &mut bool); 9] {
        [
            ("Land cover", &mut self.landcover),
            ("Boundaries", &mut self.boundaries),
            ("Rail", &mut self.rail),
            ("Labels", &mut self.labels),
            ("Route shields", &mut self.shields),
            ("Fuel and charging", &mut self.fuel),
            ("Lodging and camping", &mut self.lodging),
            ("Food", &mut self.food),
            ("Other places", &mut self.other_pois),
        ]
    }

    fn poi_shown(&self, class: &str) -> bool {
        if FUEL.contains(&class) {
            self.fuel
        } else if LODGING.contains(&class) {
            self.lodging
        } else if FOOD.contains(&class) {
            self.food
        } else {
            self.other_pois
        }
    }
}

/// The toggles resolved against a compiled style and the pack's schema. The default hides
/// nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hidden {
    /// By paint index.
    paints: Vec<bool>,
    /// By layer and class: the class's label, icon and shields.
    symbols: Vec<Vec<bool>>,
    /// By layer: its text labels.
    text: Vec<bool>,
    shields: bool,
}

impl Hidden {
    pub fn new(layers: &Layers, style: &Style, schema: &Schema) -> Self {
        let mut h = Self {
            paints: vec![false; style.paints.len()],
            symbols: schema
                .layers
                .iter()
                .map(|l| vec![false; l.classes.len()])
                .collect(),
            text: vec![false; schema.layers.len()],
            shields: !layers.shields,
        };
        for (l, layer) in schema.layers.iter().enumerate() {
            let geometry = match layer.name.as_str() {
                "landcover" => layers.landcover,
                "boundaries" => layers.boundaries,
                "rail" => layers.rail,
                _ => true,
            };
            if !geometry {
                for cs in style.classes[l].iter().flatten() {
                    for p in [Some(cs.paint), cs.tunnel, cs.bridge].into_iter().flatten() {
                        h.paints[usize::from(p)] = true;
                    }
                }
            }
            if layer.name == "pois" {
                for (c, class) in layer.classes.iter().enumerate() {
                    h.symbols[l][c] = !layers.poi_shown(&class.name);
                }
            } else {
                h.text[l] = !layers.labels;
            }
        }
        h
    }

    pub fn paint(&self, index: usize) -> bool {
        self.paints.get(index).copied().unwrap_or(false)
    }

    /// Whether a label of `layer` and `class` is hidden: a text label, or with `shields`
    /// the class's route shields.
    pub fn label(&self, layer: u8, class: u16, shields: bool) -> bool {
        let l = usize::from(layer);
        let class_hidden = self
            .symbols
            .get(l)
            .and_then(|c| c.get(usize::from(class)))
            .copied()
            .unwrap_or(false);
        class_hidden
            || match shields {
                true => self.shields,
                false => self.text.get(l).copied().unwrap_or(false),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::StyleFile;
    use omp_core::whitelist::{DEFAULT, Whitelist};

    fn style() -> (Style, Schema) {
        let schema = Schema::from_whitelist(&Whitelist::parse(DEFAULT).unwrap());
        let (style, _) = Style::compile(&StyleFile::builtin(), &schema);
        (style, schema)
    }

    fn class(schema: &Schema, layer: &str, class: &str) -> (u8, u16) {
        let (l, sl) = schema.layer(layer).unwrap();
        (l, sl.class(class).unwrap())
    }

    #[test]
    fn default_hides_nothing() {
        let (style, schema) = style();
        let h = Hidden::new(&Layers::default(), &style, &schema);
        assert!((0..style.paints.len()).all(|i| !h.paint(i)));
        let (l, c) = class(&schema, "pois", "fuel");
        assert!(!h.label(l, c, false));
        assert!(!Hidden::default().label(l, c, true));
    }

    #[test]
    fn toggles_hide_their_paints_and_labels() {
        let (style, schema) = style();
        let layers = Layers {
            rail: false,
            labels: false,
            food: false,
            ..Layers::default()
        };
        let h = Hidden::new(&layers, &style, &schema);
        let paint = |layer, class| {
            let (l, c) = self::class(&schema, layer, class);
            usize::from(style.class(l, c).unwrap().paint)
        };
        assert!(h.paint(paint("rail", "rail")));
        assert!(!h.paint(paint("roads", "motorway")));
        assert!(!h.paint(paint("landcover", "forest")));

        let (l, c) = class(&schema, "places", "city");
        assert!(h.label(l, c, false));
        let (l, c) = class(&schema, "roads", "motorway");
        assert!(h.label(l, c, false));
        assert!(!h.label(l, c, true), "shields have their own toggle");
        let (l, c) = class(&schema, "pois", "restaurant");
        assert!(h.label(l, c, false));
        let (l, c) = class(&schema, "pois", "fuel");
        assert!(!h.label(l, c, false), "POI names follow their group");
        let (l, c) = class(&schema, "pois", "airport");
        assert!(!h.label(l, c, false));
    }
}
