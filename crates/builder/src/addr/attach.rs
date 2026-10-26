//! Attaching address records to OSM streets: by the exact street key, then by the loose key
//! (phase 0), then, for records neither finds within the attach distance (phase 2 step 2.4):
//! - **alias:** with a town's name dropped from the start or end of the street name
//!   ("Oro-Medonte Line 4" → "Line 4", "Highway 24 East Norfolk County" → "Highway 24
//!   East"), the record's postal town or an admin area (level 5 and up) holding it;
//! - **fuzzy:** a street whose key differs by one edit in one word of 5+ letters ("Dixion",
//!   "Rvierside") or only in its spaces ("LeMesurier"), when it is the only such street
//!   within the distance.

use omp_core::normalize::{is_generic, loose_key, street_key, street_tokens};
use rayon::prelude::*;
use serde::Serialize;

use super::Rec;
use super::streets::{Hit, Keys, Streets};
use crate::admin::Admin;

/// How a record found its street.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    Strict,
    Loose,
    Alias,
    Fuzzy,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Strict => "strict",
            How::Loose => "loose",
            How::Alias => "alias",
            How::Fuzzy => "fuzzy",
        }
    }
}

/// One record's search: metres to the nearest way with its strict key and with its loose
/// key (looked for only when the strict one is further than `loose_from`), and the hit it
/// attaches to at the attach distance, with the key it was found under.
pub struct Found {
    pub strict_m: f64,
    pub loose_m: f64,
    pub hit: Option<(Hit, u32, How)>,
}

/// Admin areas whose names may be dropped from a street name: level 5 (Quebec regions) and up.
const ALIAS_MIN_LEVEL: u8 = 5;

/// Attaches each record within `attach_m`. `names` and `cities` are the records' street
/// names and postal towns.
#[allow(clippy::too_many_arguments)]
pub fn attach(
    recs: &[Rec],
    names: &[String],
    cities: &[String],
    streets: &Streets,
    keys: &Keys,
    admin: &Admin,
    attach_m: f64,
    loose_from: f64,
) -> Vec<Found> {
    let area_tokens: Vec<Vec<String>> = admin
        .areas
        .par_iter()
        .map(|a| street_tokens(&a.name))
        .collect();
    let city_tokens: Vec<Vec<String>> = cities.par_iter().map(|c| street_tokens(c)).collect();
    recs.par_iter()
        .map(|r| {
            let near = |k: u32, loose: bool| {
                streets
                    .nearest(k, loose, r.p)
                    .filter(|h| h.dist_m <= attach_m)
            };
            let hs = streets.nearest(r.key, false, r.p);
            let strict_m = hs.map_or(f64::INFINITY, |h| h.dist_m);
            let lk = keys.loose[r.key as usize];
            let hl = if strict_m > loose_from {
                streets.nearest(lk, true, r.p)
            } else {
                None
            };
            let loose_m = hl.map_or(f64::INFINITY, |h| h.dist_m);
            let hit = if strict_m <= attach_m {
                hs.map(|h| (h, r.key, How::Strict))
            } else if loose_m <= attach_m {
                hl.map(|h| (h, lk, How::Loose))
            } else {
                let mut aliases: Vec<&[String]> = Vec::new();
                if r.city != 0 {
                    aliases.push(&city_tokens[r.city as usize]);
                }
                for a in admin.chain(r.p) {
                    if admin.areas[a as usize].level >= ALIAS_MIN_LEVEL {
                        aliases.push(&area_tokens[a as usize]);
                    }
                }
                alias_keys(&names[r.street as usize], &aliases)
                    .into_iter()
                    .find_map(|k| {
                        let strict = keys.get(&k).and_then(|id| Some((near(id, false)?, id)));
                        strict.or_else(|| {
                            let id = keys.get(&loose_key(&k))?;
                            Some((near(id, true)?, id))
                        })
                    })
                    .map(|(h, k)| (h, k, How::Alias))
                    .or_else(|| {
                        fuzzy(streets, keys, r.key, r.p, attach_m).map(|(h, k)| (h, k, How::Fuzzy))
                    })
            };
            Found {
                strict_m,
                loose_m,
                hit,
            }
        })
        .collect()
}

/// The keys of `name` with an alias dropped from its start or end: the longest run of its
/// words at that end that is a run of an alias's words, when the run has a word of 4+
/// letters that is not a street type, direction or article, and what is left has a word
/// that is none of those either.
pub fn alias_keys(name: &str, aliases: &[&[String]]) -> Vec<String> {
    let t = street_tokens(name);
    let mut out: Vec<String> = Vec::new();
    for a in aliases {
        for lead in [true, false] {
            let run = |n: usize| if lead { &t[..n] } else { &t[t.len() - n..] };
            let Some(n) = (1..t.len())
                .rev()
                .find(|&n| a.windows(n).any(|w| w == run(n)))
            else {
                continue;
            };
            let rest = if lead { &t[n..] } else { &t[..t.len() - n] };
            let telling = run(n).iter().any(|w| {
                w.chars().count() >= 4 && !is_generic(w) && w.chars().all(char::is_alphabetic)
            });
            if telling && rest.iter().any(|w| !is_generic(w)) {
                let k = street_key(&rest.join(" "));
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// The street within `attach_m` whose strict key is [`one_edit`] from `key`, if exactly one
/// key near `p` is.
fn fuzzy(
    streets: &Streets,
    keys: &Keys,
    key: u32,
    p: [u32; 2],
    attach_m: f64,
) -> Option<(Hit, u32)> {
    let k = &keys.list[key as usize];
    let mut found = None;
    for c in streets.keys_near(p) {
        if !one_edit(k, &keys.list[c as usize]) {
            continue;
        }
        let Some(h) = streets
            .nearest(c, false, p)
            .filter(|h| h.dist_m <= attach_m)
        else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some((h, c));
    }
    found
}

/// Whether two street keys differ only in their spaces, or by one edit (a letter added,
/// dropped or changed, or two neighbouring letters swapped) in one word of 5+ letters with
/// the other words the same.
pub fn one_edit(a: &str, b: &str) -> bool {
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    if a.replace(' ', "") == b.replace(' ', "") {
        return true;
    }
    let (ta, tb): (Vec<&str>, Vec<&str>) = (a.split(' ').collect(), b.split(' ').collect());
    if ta.len() != tb.len() {
        return false;
    }
    let mut diff = ta.iter().zip(&tb).filter(|(x, y)| x != y);
    let (Some((x, y)), None) = (diff.next(), diff.next()) else {
        return false;
    };
    let long = |w: &str| w.chars().count() >= 5 && w.chars().all(char::is_alphabetic);
    long(x) && long(y) && edit1(x, y)
}

/// Whether two different words are one insertion, deletion, substitution or swap of
/// neighbouring letters apart.
fn edit1(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if a.len() == b.len() {
        let swap = pre + 1 < a.len() && a[pre] == b[pre + 1] && a[pre + 1] == b[pre];
        a[pre + 1..] == b[pre + 1..] || (swap && a[pre + 2..] == b[pre + 2..])
    } else {
        b.len() == a.len() + 1 && a[pre..] == b[pre + 1..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_edit_cases() {
        let k = street_key;
        assert!(one_edit(&k("Dixion Road"), &k("Dixon Road")));
        assert!(one_edit(
            &k("Rvierside Drive East"),
            &k("Riverside Drive East")
        ));
        assert!(one_edit(&k("Rue Deschapms"), &k("Rue Deschamps")));
        assert!(one_edit(&k("Rue Cimon"), &k("Rue Simon")));
        assert!(one_edit(&k("Avenue LeMesurier"), &k("Avenue Le Mesurier")));
        assert!(one_edit(&k("Morningdove Lane"), &k("Morning Dove Lane")));
        // Short words, numbers, two words changed, or the same key: no.
        assert!(!one_edit(&k("Oak Street"), &k("Oaks Street")));
        assert!(!one_edit(&k("Line 14"), &k("Line 15")));
        assert!(!one_edit(&k("Rembrandt Place"), &k("Rembrant Gate")));
        assert!(!one_edit(&k("Main Street"), &k("Main St")));
        assert!(!one_edit(&k("Bloxam Road"), &k("Bloxhams Road")));
    }

    #[test]
    fn aliases_dropped_from_either_end() {
        let t = |s: &str| street_tokens(s);
        let (oro, norfolk, arran) = (t("Oro-Medonte"), t("Norfolk County"), t("Arran-Elderslie"));
        let keys = |name: &str, a: &[&[String]]| alias_keys(name, a);
        assert_eq!(keys("Oro-Medonte Line 4", &[&oro]), [street_key("Line 4")]);
        assert_eq!(
            keys("Highway 24 East Norfolk County", &[&norfolk]),
            [street_key("Highway 24 East")]
        );
        assert_eq!(
            keys("Sideroad 5 Elderslie", &[&arran]),
            [street_key("Sideroad 5")]
        );
        // Nothing telling would be left, or nothing telling is dropped.
        let kingston = t("Kingston");
        assert!(keys("Kingston Road", &[&kingston]).is_empty());
        let north_bay = t("North Bay");
        assert!(keys("North Main Street", &[&north_bay]).is_empty());
    }
}
