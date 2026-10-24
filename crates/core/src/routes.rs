//! The `routes` prop of road features: the road routes a way belongs to, for shields.
//!
//! A value is entries `network=ref` joined by `;`, most important first, e.g.
//! `US:I=75;US:US=23`. Networks are OSM `network` tags (`US:I`, `US:OH`, `CA:ON:primary`,
//! `CA:transcanada`) from `type=route` + `route=road` relations. Ways in no such relation fall
//! back to their own `ref` tag, parsed by [`from_ref`]: the network is then guessed from the
//! designator (`I 75` → `US:I`, `OH 4` → `US:OH`), kept as written when it is not known
//! (`CR 12` → `CR`), or empty for a bare number (`=138`).

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route<'a> {
    pub network: &'a str,
    pub reference: &'a str,
}

/// Parses a `routes` value. Entries without `=` are taken as a ref with no network.
pub fn parse(value: &str) -> impl Iterator<Item = Route<'_>> {
    value
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| match e.split_once('=') {
            Some((network, reference)) => Route { network, reference },
            None => Route {
                network: "",
                reference: e,
            },
        })
}

/// Sorts `routes` by importance, drops duplicates, and joins them into a `routes` value.
pub fn format(routes: &mut Vec<(String, String)>) -> String {
    routes.sort_by(|a, b| compare(&a.0, &a.1, &b.0, &b.1));
    routes.dedup();
    let mut out = String::new();
    for (network, reference) in routes.iter() {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(network);
        out.push('=');
        out.push_str(reference);
    }
    out
}

fn compare(na: &str, ra: &str, nb: &str, rb: &str) -> Ordering {
    priority(na)
        .cmp(&priority(nb))
        .then_with(|| na.cmp(nb))
        .then_with(|| compare_refs(ra, rb))
}

/// Refs by their leading number, then as text, so `9` comes before `75` and `75A`.
fn compare_refs(a: &str, b: &str) -> Ordering {
    let num = |s: &str| -> Option<u64> {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        s[..end].parse().ok()
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Lower is more important: 0 Interstates and the Trans-Canada, 1 US routes and Interstate
/// business loops, 2 states and provinces (and US route variants), 3 counties and other
/// networks under a state, 4 anything else (other countries, guessed or unknown networks).
pub fn priority(network: &str) -> u8 {
    let parts: Vec<&str> = network.split(':').collect();
    match parts[..] {
        ["US", "I"] | ["CA", "transcanada"] => 0,
        ["US", "US"] | ["US", "I", ..] | ["MX", _] if is_federal(&parts) => 1,
        ["US", "US", ..] => 2,
        ["US", s] | ["CA", s, ..] | ["MX", s, ..] if s.len() == 2 || s.len() == 3 => 2,
        ["US", s, _, ..] if s.len() == 2 => 3,
        _ => 4,
    }
}

fn is_federal(parts: &[&str]) -> bool {
    match parts {
        // Mexico's federal highways are mostly `MX:MX` (North America, 2026-10), some `MX:MEX`.
        ["MX", s] => matches!(*s, "MX" | "MEX" | "Federal"),
        _ => true,
    }
}

#[rustfmt::skip]
const US_STATES: &[&str] = &[
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "DC", "FL", "GA", "HI", "ID", "IL", "IN",
    "IA", "KS", "KY", "LA", "ME", "MD", "MA", "MI", "MN", "MS", "MO", "MT", "NE", "NV", "NH",
    "NJ", "NM", "NY", "NC", "ND", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT",
    "VT", "VA", "WA", "WV", "WI", "WY",
];
#[rustfmt::skip]
const PROVINCES: &[&str] = &[
    "AB", "BC", "MB", "NB", "NL", "NS", "NT", "NU", "ON", "PE", "QC", "SK", "YT",
];

/// The network a ref designator stands for, e.g. `I` → `US:I`.
fn designator_network(d: &str) -> Option<String> {
    let d = d.trim_end_matches('.').replace('.', "");
    let net = match d.as_str() {
        "I" | "IH" | "INTERSTATE" => "US:I".into(),
        "US" | "USHWY" | "USH" => "US:US".into(),
        "TCH" => "CA:transcanada".into(),
        s if US_STATES.contains(&s) => format!("US:{s}"),
        s if PROVINCES.contains(&s) => format!("CA:{s}"),
        _ => return None,
    };
    Some(net)
}

/// Routes from a way's `ref` tag, e.g. `I 75;US 23` → `[(US:I, 75), (US:US, 23)]`.
///
/// Each `;`-separated ref is split at spaces and hyphens. The first word that starts with a
/// digit begins the route number; the words before it are the designator. A designator that
/// names a known network becomes that network; any other is kept, upper-cased, as the
/// network (`CR 12` → `CR`, `FM 1960` → `FM`). A ref with no designator has an empty network,
/// and one with no number is kept whole as the ref, with an empty network.
pub fn from_ref(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for r in value.split(';').map(str::trim).filter(|r| !r.is_empty()) {
        let words: Vec<&str> = r.split([' ', '-']).filter(|w| !w.is_empty()).collect();
        let Some(n) = words
            .iter()
            .position(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        else {
            out.push((String::new(), r.replace(['=', ';'], " ")));
            continue;
        };
        let designator = words[..n].join("").to_uppercase();
        let number = words[n..].join(" ").replace('=', " ");
        let network = if designator.is_empty() {
            String::new()
        } else {
            designator_network(&designator).unwrap_or_else(|| designator.replace([':', '='], ""))
        };
        out.push((network, number));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn formats_by_priority() {
        let mut r = pairs(&[
            ("US:OH", "4"),
            ("US:US", "23"),
            ("US:I", "75"),
            ("US:OH:FRA", "12"),
            ("US:US", "23"),
            ("US:I", "270"),
            ("CR", "5"),
        ]);
        assert_eq!(
            format(&mut r),
            "US:I=75;US:I=270;US:US=23;US:OH=4;US:OH:FRA=12;CR=5"
        );
        let mut ca = pairs(&[("CA:ON:primary", "401"), ("CA:transcanada", "1")]);
        assert_eq!(format(&mut ca), "CA:transcanada=1;CA:ON:primary=401");
        assert_eq!(priority("US:I:Business"), 1);
        assert_eq!(priority("US:US:Truck"), 2);
        assert_eq!(priority("MX:MEX"), 1);
        assert_eq!(priority("MX:MX"), 1);
        assert_eq!(priority("MX:SON"), 2);
        assert_eq!(priority("DE:BAB"), 4);
    }

    #[test]
    fn parses_values() {
        let v: Vec<Route> = parse("US:I=75;=138;401").collect();
        assert_eq!(v[0].network, "US:I");
        assert_eq!(v[0].reference, "75");
        assert_eq!((v[1].network, v[1].reference), ("", "138"));
        assert_eq!((v[2].network, v[2].reference), ("", "401"));
    }

    #[test]
    fn refs_fall_back_to_designators() {
        assert_eq!(
            from_ref("I 75;US 23"),
            pairs(&[("US:I", "75"), ("US:US", "23")])
        );
        assert_eq!(from_ref("OH 4"), pairs(&[("US:OH", "4")]));
        assert_eq!(from_ref("QC-138"), pairs(&[("CA:QC", "138")]));
        assert_eq!(from_ref("CR 12"), pairs(&[("CR", "12")]));
        assert_eq!(from_ref("I-75 Business"), pairs(&[("US:I", "75 Business")]));
        assert_eq!(from_ref("U.S. 50"), pairs(&[("US:US", "50")]));
        assert_eq!(from_ref("401"), pairs(&[("", "401")]));
        assert_eq!(from_ref("Main"), pairs(&[("", "Main")]));
        assert_eq!(from_ref(" ; "), pairs(&[]));
    }
}
