//! The town shown for a street, place or POI (decision 4): the postal town its addresses
//! give, else the admin 8 city, town or village it lies in, else the nearest place node.

use omp_core::normalize::tokens;
use serde::Serialize;

use super::Admin;
use crate::measure::geom::P;

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
