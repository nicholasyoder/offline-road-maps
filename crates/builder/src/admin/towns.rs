//! The town shown for a street, place or POI (decision 4): the postal town its addresses
//! give, else the admin 8 city, town or village it lies in, else the nearest place node.

use std::collections::HashMap;

use omp_core::normalize::tokens;
use serde::Serialize;

use super::Admin;
use crate::addr::streets::cell;
use crate::geom::P;

/// How far a place node may be from a street and still name its town.
pub const PLACE_MAX_M: f64 = 25_000.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The postal town most of its addresses give.
    Addresses,
    /// The admin 8 city, town or village it lies in.
    Admin,
    /// The postal town most streets around it have.
    Neighbours,
    /// The nearest `place=city|town|village` node, else a hamlet.
    Place,
    None,
}

/// The fallbacks, in the order tried for a street with no postal town (decision 4, revised
/// in 2.1). Tried on the streets that have one, as if they had none, this order names their
/// postal town for 93.8% of North America's streets; the area first, then the nearest
/// place (the plan's first order) for 59.4%.
pub const FALLBACK: [Method; 5] = [
    Method::LocalNeighbours,
    Method::Area,
    Method::Neighbours,
    Method::MajorPlace,
    Method::AnyPlace,
];

/// One way to name a street's town without its own addresses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The admin 8 city, town or village it lies in.
    Area,
    /// The postal town most streets within about 1 km give, counting only streets in the
    /// same municipality or township (the deepest level 7 or 8 area).
    LocalNeighbours,
    /// The postal town most streets within about 1 km give.
    Neighbours,
    /// The nearest city, town or village node.
    MajorPlace,
    /// The nearest place node, hamlets included.
    AnyPlace,
}

impl Method {
    pub fn source(self) -> Source {
        match self {
            Method::Area => Source::Admin,
            Method::LocalNeighbours | Method::Neighbours => Source::Neighbours,
            Method::MajorPlace | Method::AnyPlace => Source::Place,
        }
    }
}

/// A street for the fallbacks: its middle point, admin chain and postal town (display
/// name and [`key`]), if its addresses give one.
pub struct Located {
    pub p: P,
    pub chain: Vec<u32>,
    pub postal: Option<(String, String)>,
}

/// The fallbacks over a set of streets, whose postal towns are each other's neighbours.
pub struct Fallbacks<'a> {
    admin: &'a Admin,
    rows: &'a [Located],
    /// Streets with a postal town, by the ~1 km grid of [`cell`].
    near: HashMap<(i32, i32), Vec<usize>>,
    /// The deepest level 7 or 8 area of each street.
    local: Vec<Option<u32>>,
}

impl<'a> Fallbacks<'a> {
    pub fn new(admin: &'a Admin, rows: &'a [Located]) -> Self {
        let mut near: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, s) in rows.iter().enumerate() {
            if s.postal.is_some() {
                near.entry(cell(s.p)).or_default().push(i);
            }
        }
        let local = rows
            .iter()
            .map(|s| {
                s.chain
                    .iter()
                    .copied()
                    .filter(|&a| matches!(admin.areas[a as usize].level, 7 | 8))
                    .max_by_key(|&a| admin.areas[a as usize].level)
            })
            .collect();
        Self {
            admin,
            rows,
            near,
            local,
        }
    }

    fn neighbours(&self, i: usize, same_area: bool) -> Option<(String, String)> {
        if same_area && self.local[i].is_none() {
            return None;
        }
        let (cx, cy) = cell(self.rows[i].p);
        let mut counts: HashMap<&str, (u32, &str)> = HashMap::new();
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &j in self.near.get(&(x, y)).into_iter().flatten() {
                    if j == i || (same_area && self.local[j] != self.local[i]) {
                        continue;
                    }
                    if let Some((name, k)) = &self.rows[j].postal {
                        counts.entry(k).or_insert((0, name)).0 += 1;
                    }
                }
            }
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(a.0)))
            .map(|(k, (_, name))| (name.to_owned(), k.to_owned()))
    }

    /// The town `m` names for street `i`, as display name and key.
    pub fn answer(&self, i: usize, m: Method) -> Option<(String, String)> {
        let s = &self.rows[i];
        let name = match m {
            Method::Area => area_town(self.admin, &s.chain),
            Method::LocalNeighbours => return self.neighbours(i, true),
            Method::Neighbours => return self.neighbours(i, false),
            Method::MajorPlace => place_town(self.admin, s.p, true),
            Method::AnyPlace => place_town(self.admin, s.p, false),
        }?;
        let k = key(&name);
        Some((name, k))
    }

    /// The first method in `order` that answers, and its answer.
    pub fn first(&self, i: usize, order: &[Method]) -> Option<(Method, (String, String))> {
        order
            .iter()
            .find_map(|&m| self.answer(i, m).map(|a| (m, a)))
    }
}

/// Matching key of a town name: folded tokens, so "SAINT-THÉOPHILE" meets "Saint-Théophile".
pub fn key(name: &str) -> String {
    tokens(name).join(" ")
}

/// "SAINT-THÉOPHILE" → "Saint-Théophile", "NORTH CANTON" → "North Canton". Names already
/// in mixed case are left alone.
pub fn title_case(s: &str) -> String {
    if s.chars().any(char::is_lowercase) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    // Letters so far in the current word, and whether the next letter starts a word.
    let (mut letters, mut start) = (0, true);
    for c in s.chars() {
        if c.is_alphabetic() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            letters += 1;
            start = false;
        } else {
            // "O'Fallon" and "L'Assomption" capitalize after the apostrophe; "Mary's" does not.
            start = !matches!(c, '\'' | '’') || letters == 1;
            letters = 0;
            out.push(c);
        }
    }
    out
}

/// Words sources put after a town's name to say what kind of municipality it is
/// ("Brunswick City", "Westfield Center Vil").
const KIND_WORDS: &[&str] = &[
    "city", "village", "vil", "vlg", "town", "twp", "township", "borough", "boro",
];

/// The display form of a postal town spelled `raw` by the address source: the name of an
/// admin area in `chain` or of a place within [`PLACE_MAX_M`] with the same key, also
/// after dropping a trailing [`KIND_WORDS`] word, else `raw` in title case.
pub fn display(admin: &Admin, raw: &str, chain: &[u32], p: P) -> String {
    let k = key(raw);
    let short = k
        .rsplit_once(' ')
        .filter(|(_, last)| KIND_WORDS.contains(last))
        .map(|(head, _)| head.to_owned());
    for k in std::iter::once(&k).chain(short.as_ref()) {
        if let Some(a) = chain
            .iter()
            .rev()
            .map(|&a| &admin.areas[a as usize])
            .find(|a| key(&a.name) == *k)
        {
            return a.name.clone();
        }
        if let Some(i) = admin.nearest_place_named(p, k, PLACE_MAX_M) {
            return admin.places[i as usize].name.clone();
        }
    }
    title_case(raw)
}

/// The admin 8 city, town or village in `chain`.
pub fn area_town(admin: &Admin, chain: &[u32]) -> Option<String> {
    admin
        .at_level(chain, 8)
        .filter(|&a| admin.towns[a as usize])
        .map(|a| admin.areas[a as usize].name.clone())
}

/// The nearest place node within [`PLACE_MAX_M`]; with `major`, only cities, towns and
/// villages.
pub fn place_town(admin: &Admin, p: P, major: bool) -> Option<String> {
    let i = if major {
        admin.nearest_major_place(p, PLACE_MAX_M)
    } else {
        admin.nearest_place(p, PLACE_MAX_M)
    };
    i.map(|i| admin.places[i as usize].name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_case_of_upper_names() {
        assert_eq!(title_case("SAINT-THÉOPHILE"), "Saint-Théophile");
        assert_eq!(title_case("NORTH CANTON"), "North Canton");
        assert_eq!(title_case("O'FALLON"), "O'Fallon");
        assert_eq!(title_case("L'ASSOMPTION"), "L'Assomption");
        assert_eq!(title_case("ST MARY'S"), "St Mary's");
        assert_eq!(title_case("Columbus"), "Columbus");
        assert_eq!(key("SAINT-THÉOPHILE"), key("Saint-Théophile"));
    }
}
