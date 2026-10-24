//! Text normalization for search and address matching: case folding, accent stripping,
//! tokenizing, street-name keys and house-number parsing.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Lowercases, strips accents (NFKD minus combining marks) and splits on anything that is
/// not a letter or digit. "Saint-Jérôme" gives `["saint", "jerome"]`.
pub fn tokens(text: &str) -> Vec<String> {
    let folded: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Abbreviations and variants of street types and common name words, as
/// `(variant, canonical)`. Mostly USPS Publication 28 (appendix C1) and Canada Post. A few
/// position-dependent ones (`st`, `ste`, `dr` and the directionals) are handled in
/// [`street_tokens`] instead.
#[rustfmt::skip]
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("aly", "alley"), ("anx", "annex"), ("arc", "arcade"), ("av", "avenue"), ("ave", "avenue"),
    ("avn", "avenue"), ("bch", "beach"), ("bd", "boulevard"), ("blf", "bluff"),
    ("blvd", "boulevard"), ("boul", "boulevard"), ("br", "branch"), ("brg", "bridge"),
    ("brk", "brook"), ("byp", "bypass"), ("byps", "bypass"), ("carref", "carrefour"),
    ("ch", "chemin"), ("cir", "circle"), ("cl", "close"), ("cmn", "common"), ("cor", "corner"),
    ("cres", "crescent"), ("crois", "croissant"), ("crk", "creek"), ("crse", "course"),
    ("crst", "crest"), ("crt", "court"), ("cswy", "causeway"), ("ct", "court"),
    ("ctr", "center"), ("centre", "center"), ("cv", "cove"), ("cyn", "canyon"), ("dl", "dale"),
    ("drv", "drive"), ("expwy", "expressway"), ("expy", "expressway"), ("exwy", "expressway"), ("ext", "extension"), ("fld", "field"),
    ("flds", "fields"), ("fls", "falls"), ("frst", "forest"), ("ft", "fort"),
    ("fwy", "freeway"), ("gdns", "gardens"), ("gln", "glen"), ("grn", "green"),
    ("grv", "grove"), ("gtwy", "gateway"), ("hbr", "harbor"), ("hl", "hill"), ("hls", "hills"),
    ("holw", "hollow"), ("hts", "heights"), ("hvn", "haven"), ("hwy", "highway"),
    ("imp", "impasse"), ("jct", "junction"), ("jr", "junior"), ("knl", "knoll"),
    ("ldg", "lodge"), ("lk", "lake"), ("lks", "lakes"), ("lndg", "landing"), ("ln", "lane"),
    ("mdw", "meadow"), ("mdws", "meadows"), ("ml", "mill"), ("mnr", "manor"),
    ("mont", "montee"), ("mt", "mount"), ("mtn", "mountain"), ("orch", "orchard"),
    ("ovlk", "overlook"), ("pk", "park"), ("pkwy", "parkway"), ("pky", "parkway"),
    ("pl", "place"), ("pln", "plain"), ("plz", "plaza"), ("pne", "pine"), ("pnes", "pines"),
    ("priv", "private"), ("prom", "promenade"), ("pt", "point"), ("pvt", "private"),
    ("rd", "road"),
    ("rdg", "ridge"), ("rg", "rang"), ("riv", "river"), ("rle", "ruelle"), ("rnch", "ranch"),
    ("rt", "route"), ("rte", "route"), ("sent", "sentier"), ("shr", "shore"), ("smt", "summit"),
    ("spg", "spring"), ("sq", "square"), ("sta", "station"), ("str", "street"),
    ("terr", "terrace"), ("ter", "terrace"), ("tpke", "turnpike"), ("trce", "trace"),
    ("trl", "trail"), ("tsse", "terrasse"), ("vlg", "village"), ("vly", "valley"),
    ("vw", "view"), ("vis", "vista"), ("wy", "way"), ("xing", "crossing"),
];

/// Canonical words that name the kind of street rather than the street. Dropped by
/// [`loose_key`].
#[rustfmt::skip]
const TYPES: &[&str] = &[
    "alley", "allee", "autoroute", "avenue", "boulevard", "bypass", "carrefour", "causeway",
    "chemin", "circle", "close", "court", "cove", "crescent", "croissant", "drive",
    "expressway", "freeway", "highway", "impasse", "lane", "loop", "montee", "parkway", "pass",
    "pike", "place", "plaza", "private", "promenade", "rang", "road", "route", "rue", "ruelle",
    "run",
    "sentier", "square", "street", "terrace", "terrasse", "trail", "turnpike", "way",
];

/// Directional abbreviations, only expanded as the first or last word (`o` is French
/// "ouest", and inside a name it is more likely an initial).
#[rustfmt::skip]
const DIRECTION_ABBREVIATIONS: &[(&str, &str)] = &[
    ("n", "north"), ("s", "south"), ("e", "east"), ("w", "west"), ("o", "west"),
    ("ne", "northeast"), ("nw", "northwest"), ("se", "southeast"), ("sw", "southwest"),
    ("no", "northwest"), ("so", "southwest"),
];

/// Directions in full; French ones map to English so "Est" and "E" agree. Carriageway
/// bounds ("I-71 SB") count as directions too.
#[rustfmt::skip]
const DIRECTIONS: &[(&str, &str)] = &[
    ("north", "north"), ("south", "south"), ("east", "east"), ("west", "west"),
    ("northeast", "northeast"), ("northwest", "northwest"), ("southeast", "southeast"),
    ("southwest", "southwest"), ("nord", "north"), ("sud", "south"), ("est", "east"),
    ("ouest", "west"), ("nb", "northbound"), ("sb", "southbound"), ("eb", "eastbound"),
    ("wb", "westbound"), ("northbound", "northbound"), ("southbound", "southbound"),
    ("eastbound", "eastbound"), ("westbound", "westbound"),
];

#[rustfmt::skip]
const ORDINALS: &[(&str, u32)] = &[
    ("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("fifth", 5), ("sixth", 6),
    ("seventh", 7), ("eighth", 8), ("ninth", 9), ("tenth", 10), ("eleventh", 11),
    ("twelfth", 12), ("thirteenth", 13), ("fourteenth", 14), ("fifteenth", 15),
    ("sixteenth", 16), ("seventeenth", 17), ("eighteenth", 18), ("nineteenth", 19),
    ("twentieth", 20), ("thirtieth", 30), ("fortieth", 40), ("fiftieth", 50),
    ("sixtieth", 60), ("seventieth", 70), ("eightieth", 80), ("ninetieth", 90),
];

#[rustfmt::skip]
const TENS: &[(&str, u32)] = &[
    ("twenty", 20), ("thirty", 30), ("forty", 40), ("fifty", 50), ("sixty", 60),
    ("seventy", 70), ("eighty", 80), ("ninety", 90),
];

/// Route designators: the words before a route number, and the short form they become, so
/// "State Rte 161", "SR 161" and "OH 161" give the same key. Matched against the whole
/// name after abbreviations are expanded.
#[rustfmt::skip]
const DESIGNATORS: &[(&[&str], &str)] = &[
    (&["state", "route"], "sr"), (&["state", "highway"], "sr"), (&["state"], "sr"),
    (&["sr"], "sr"), (&["sh"], "sr"), (&["oh"], "sr"), (&["ohio"], "sr"),
    (&["us"], "us"), (&["us", "route"], "us"), (&["us", "highway"], "us"),
    (&["united", "states", "highway"], "us"), (&["united", "states", "route"], "us"),
    (&["county", "road"], "cr"), (&["county", "route"], "cr"), (&["county", "highway"], "cr"),
    (&["county"], "cr"), (&["cr"], "cr"), (&["co", "road"], "cr"), (&["co", "route"], "cr"),
    (&["co", "highway"], "cr"), (&["co"], "cr"),
    (&["township", "road"], "tr"), (&["township", "highway"], "tr"), (&["township"], "tr"),
    (&["twp", "road"], "tr"), (&["twp", "highway"], "tr"), (&["twp"], "tr"), (&["tr"], "tr"),
    (&["th"], "tr"),
    (&["interstate"], "i"), (&["interstate", "highway"], "i"), (&["i"], "i"),
    (&["highway"], "highway"), (&["kings", "highway"], "highway"),
    (&["king", "s", "highway"], "highway"), (&["provincial", "highway"], "highway"),
    (&["route"], "route"),
];

/// Words written both joined and apart ("Middle Sideroad", "Middle Side Road").
const COMPOUNDS: &[(&str, [&str; 2])] = &[
    ("sideroad", ["side", "road"]),
    ("sdrd", ["side", "road"]),
    ("townline", ["town", "line"]),
    ("twnln", ["town", "line"]),
];

/// English and French articles, dropped by [`loose_key`] ("Crois. de la Pétanque").
const ARTICLES: &[&str] = &["d", "de", "des", "du", "l", "la", "le", "les", "the"];

/// Words that make up route designators; [`loose_key`] drops them from numbered names.
const DESIGNATOR_WORDS: &[&str] = &[
    "co",
    "county",
    "cr",
    "i",
    "interstate",
    "sr",
    "state",
    "township",
    "tr",
    "twp",
    "us",
];

/// Words that may follow "st" when it means State ("ST RTE 180").
const ROUTE_WORDS: &[&str] = &["highway", "hwy", "route", "rt", "rte"];

fn lookup<T: Copy>(table: &[(&str, T)], word: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == word).map(|(_, v)| *v)
}

/// True for a canonical direction word, as [`street_tokens`] produces them.
pub fn is_direction(word: &str) -> bool {
    DIRECTIONS.iter().any(|(_, v)| *v == word)
}

/// The canonical words of a street name, in their original order: abbreviations expanded,
/// French directions in English and ordinals as plain numbers ("Twenty-First" and "21st"
/// both give "21").
pub fn street_tokens(name: &str) -> Vec<String> {
    let mut t: Vec<String> = tokens(name)
        .into_iter()
        .flat_map(|w| match lookup(COMPOUNDS, &w) {
            Some(parts) => parts.iter().map(|p| (*p).to_owned()).collect(),
            None => vec![w],
        })
        .collect();
    let n = t.len();
    for (i, w) in t.iter_mut().enumerate() {
        let canon = if i == 0 || i + 1 == n {
            lookup(DIRECTION_ABBREVIATIONS, w)
        } else {
            None
        }
        .or_else(|| lookup(DIRECTIONS, w));
        if let Some(c) = canon {
            *w = c.to_owned();
        }
    }
    // "st" is State before a route word, Street as the last word before any directions,
    // and Saint elsewhere.
    let last = t.iter().rposition(|w| !is_direction(w)).unwrap_or(0);
    let raw = t.clone();
    for (i, w) in t.iter_mut().enumerate() {
        let next_is_route = raw
            .get(i + 1)
            .is_some_and(|x| ROUTE_WORDS.contains(&x.as_str()));
        let c = match (w.as_str(), i == last) {
            ("st", _) if next_is_route => Some("state"),
            ("st", true) => Some("street"),
            ("st", false) => Some("saint"),
            ("ste", false) => Some("sainte"),
            ("dr", true) => Some("drive"),
            (other, _) => lookup(ABBREVIATIONS, other),
        };
        if let Some(c) = c {
            *w = c.to_owned();
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(t.len());
    for w in t {
        if let Some(n) = lookup(ORDINALS, &w) {
            match out.last().and_then(|p| lookup(TENS, p)) {
                Some(tens) if n < 10 => {
                    out.pop();
                    out.push((tens + n).to_string());
                }
                _ => out.push(n.to_string()),
            }
        } else {
            out.push(strip_ordinal(w));
        }
    }
    out
}

/// "21st" and "21e" → "21"; anything else unchanged.
fn strip_ordinal(w: String) -> String {
    let digits = w.bytes().take_while(u8::is_ascii_digit).count();
    let ordinal = matches!(
        &w[digits..],
        "st" | "nd" | "rd" | "th" | "e" | "er" | "re" | "ere" | "eme" | "ieme"
    );
    if digits > 0 && ordinal {
        w[..digits].to_owned()
    } else {
        w
    }
}

/// Shortens a route designator: `["state", "route", "161"]` → `["sr", "161"]`.
fn designator(t: Vec<String>) -> Vec<String> {
    if t.len() < 2 || !t[t.len() - 1].starts_with(|c: char| c.is_ascii_digit()) {
        return t;
    }
    let prefix = &t[..t.len() - 1];
    match DESIGNATORS
        .iter()
        .find(|(words, _)| words.iter().copied().eq(prefix.iter().map(String::as_str)))
    {
        Some((_, short)) => vec![(*short).to_owned(), t[t.len() - 1].clone()],
        None => t,
    }
}

/// The matching key of a street name: its canonical words in order, route designators
/// shortened ("State Route 161" → "sr 161"), and directions moved to the end and sorted,
/// so "N High St" and "High Street North" agree.
pub fn street_key(name: &str) -> String {
    let (dirs, words): (Vec<String>, Vec<String>) = street_tokens(name)
        .into_iter()
        .partition(|w| is_direction(w));
    let mut dirs = dirs;
    dirs.sort_unstable();
    let mut t = designator(words);
    t.extend(dirs);
    t.join(" ")
}

/// A looser key from a [`street_key`], for a second matching attempt: street types,
/// directions, articles and (in numbered names) route designators dropped, a plural "s"
/// removed from each word, and the words sorted and run together. So "Valleyvista Dr" meets
/// "Valley Vista Drive", "Jackies Bend" meets "Jackie's Bend", "RD 208" meets "Township
/// Road 208" and "Line 14" meets "14th Line". If nothing would be left, the key itself with
/// spaces removed.
pub fn loose_key(key: &str) -> String {
    let numbered = key
        .split(' ')
        .any(|w| w.starts_with(|c: char| c.is_ascii_digit()));
    let mut words: Vec<&str> = key
        .split(' ')
        .filter(|w| {
            !TYPES.contains(w)
                && !is_direction(w)
                && !ARTICLES.contains(w)
                && *w != "s"
                && !(numbered && DESIGNATOR_WORDS.contains(w))
        })
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if w.len() > 3 && !stem.ends_with('s') => stem,
            _ => w,
        })
        .collect();
    words.sort_unstable();
    if words.is_empty() {
        key.replace(' ', "")
    } else {
        words.concat()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NumberKind {
    /// "123".
    Plain,
    /// "123A", "123 B", "12 bis".
    Letter,
    /// "123 1/2", "123½", "123.5".
    Fraction,
    /// "123-125".
    Range,
}

impl NumberKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Letter => "letter",
            Self::Fraction => "fraction",
            Self::Range => "range",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HouseNumber {
    pub number: u32,
    /// Canonical suffix: "" for plain numbers, else e.g. "A", "BIS", "1/2" or "-125".
    pub suffix: String,
    pub kind: NumberKind,
}

/// Parses a house number. Returns `None` for anything that is not a number with an
/// optional letter, fraction or range suffix (e.g. "N1234", "12-A-3", "Lot 5"); callers
/// count those as normalization exceptions.
pub fn house_number(raw: &str) -> Option<HouseNumber> {
    let s = raw.trim().trim_end_matches('.').replace('½', " 1/2");
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let number: u32 = s[..digits].parse().ok()?;
    let rest = &s[digits..];
    let trimmed = rest.trim_start_matches([' ', '-']);
    let hn = |suffix: String, kind| {
        Some(HouseNumber {
            number,
            suffix,
            kind,
        })
    };
    if trimmed.is_empty() {
        return hn(String::new(), NumberKind::Plain);
    }
    let upper = trimmed.to_uppercase();
    if upper.chars().count() == 1 && upper.chars().all(|c| c.is_ascii_alphabetic())
        || upper == "BIS"
        || upper == "TER"
    {
        return hn(upper, NumberKind::Letter);
    }
    if rest == ".5" {
        return hn("1/2".into(), NumberKind::Fraction);
    }
    if let Some((a, b)) = trimmed.split_once('/')
        && rest.starts_with(' ')
        && !a.is_empty()
        && !b.is_empty()
        && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
    {
        return hn(format!("{a}/{b}"), NumberKind::Fraction);
    }
    let after_dash = rest.trim_start().strip_prefix(['-', '–'])?.trim_start();
    if !after_dash.is_empty() && after_dash.bytes().all(|c| c.is_ascii_digit()) {
        return hn(format!("-{after_dash}"), NumberKind::Range);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_case() {
        assert_eq!(tokens("Saint-Jérôme"), ["saint", "jerome"]);
        assert_eq!(tokens("  N. High St. "), ["n", "high", "st"]);
        assert_eq!(
            tokens("Côte-des-Neiges–Notre-Dame"),
            ["cote", "des", "neiges", "notre", "dame"]
        );
    }

    #[test]
    fn street_keys_agree_across_spellings() {
        let same = |a: &str, b: &str| assert_eq!(street_key(a), street_key(b), "{a} vs {b}");
        same("N High St", "North High Street");
        same("High St N", "N High Street");
        same("STATE RTE 247", "State Route 247");
        same("SR 161", "OH 161");
        same("US Hwy 23", "US 23");
        same("Co Rd 12", "County Road 12");
        same("Twenty-First Street", "21st St");
        same("Rue Ste-Catherine O", "rue Sainte-Catherine Ouest");
        same("Boul. St-Laurent", "Boulevard Saint-Laurent");
        same("Ch du Lac-Wilson", "Chemin du Lac-Wilson");
        same("ST RTE 180", "SR 180");
        same("N ST RT 741", "State Route 741 North");
        same("SH 231", "SR 231");
        same("S 6 ST", "South 6th Street");
        same("7e Rue E", "7e Rue Est");
        same("1re Avenue", "1 Avenue");
        same("Hwy 7", "Highway 7");
        same(
            "Dr Martin Luther King Jr Dr",
            "Dr Martin Luther King Junior Drive",
        );
        assert_ne!(street_key("N High St"), street_key("S High St"));
        assert_ne!(
            street_key("St Clair Ave"),
            street_key("Clair Street Avenue")
        );
    }

    #[test]
    fn loose_keys_drop_types_and_directions() {
        let loose = |a: &str| loose_key(&street_key(a));
        assert_eq!(loose("Main St W"), "main");
        assert_eq!(loose("Main Avenue"), "main");
        assert_eq!(loose("North Street"), "streetnorth");
        assert_eq!(loose("VALLEYVISTA DR"), loose("Valley Vista Drive"));
        assert_eq!(loose("JACKIES BEND"), loose("Jackie's Bend"));
        assert_eq!(
            loose("BIG RUN BLUFF BLVD"),
            loose("Big Run Bluffs Boulevard")
        );
        assert_eq!(loose("RD 208"), loose("Township Road 208"));
        assert_eq!(loose("E TR 44"), loose("Township Road 44"));
        assert_eq!(loose("du Lac-Wilson CH"), loose("Chemin du Lac-Wilson"));
        assert_eq!(loose("Line 14"), loose("14th Line"));
        assert_eq!(loose("Sideroad Schafer"), loose("Schafer Side Road"));
        assert_eq!(loose("PETANQUE, CROIS DE LA"), loose("Pétanque Crescent"));
        assert_eq!(loose("CHARTLEY PRIV"), loose("Chartley Private"));
        assert_eq!(
            loose("Townline Osprey The Blue Mountains"),
            loose("Osprey-The Blue Mountains Townline")
        );
        assert_ne!(loose("N High St"), loose("Highland Ave"));
    }

    #[test]
    fn house_numbers() {
        let hn = |s: &str| house_number(s).map(|h| (h.number, h.suffix, h.kind));
        use NumberKind::*;
        assert_eq!(hn("123"), Some((123, String::new(), Plain)));
        assert_eq!(hn(" 0123 "), Some((123, String::new(), Plain)));
        assert_eq!(hn("123a"), Some((123, "A".into(), Letter)));
        assert_eq!(hn("123 B"), Some((123, "B".into(), Letter)));
        assert_eq!(hn("12-A"), Some((12, "A".into(), Letter)));
        assert_eq!(hn("12 bis"), Some((12, "BIS".into(), Letter)));
        assert_eq!(hn("123 1/2"), Some((123, "1/2".into(), Fraction)));
        assert_eq!(hn("123½"), Some((123, "1/2".into(), Fraction)));
        assert_eq!(hn("17.5"), Some((17, "1/2".into(), Fraction)));
        assert_eq!(hn("2100."), Some((2100, String::new(), Plain)));
        assert_eq!(hn("123-125"), Some((123, "-125".into(), Range)));
        assert_eq!(hn("123 - 125"), Some((123, "-125".into(), Range)));
        for bad in [
            "",
            "N1234",
            "Lot 5",
            "12-A-3",
            "12 34",
            "1234567890",
            "123/4",
        ] {
            assert_eq!(hn(bad), None, "{bad}");
        }
    }
}
