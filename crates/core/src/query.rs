//! Query parsing (phase 2 step 2.6): the words of a query, with a leading house number, a
//! unit and a postal code taken out. Which words name the street or place and which the
//! town or state is left to [`crate::find`], which tries the plausible readings.

use crate::normalize::{self, HouseNumber, NumberKind};

/// A word of the query as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// A comma follows it.
    pub comma: bool,
}

/// A reading of a leading house number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    pub number: HouseNumber,
    /// The unit it gives too: Canadian "4-123" is unit 4 at number 123.
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The words left: street or place, then town and state, in order.
    pub words: Vec<Word>,
    /// Readings of the first word as a house number, most likely first; empty if it is not
    /// one. The words then start after it.
    pub numbers: Vec<Number>,
    /// The words read as the number, for the readings without one ("100 Mile House").
    pub lead: Vec<Word>,
    /// "Apt 4", "Unit 101", "#4", "app. 4": the unit as written after its marker.
    pub unit: Option<String>,
    /// A postal code after other words, in display form.
    pub postcode: Option<String>,
    /// The last word is still being typed.
    pub prefix: bool,
}

/// Words that introduce a unit; "ste" (Suite) only before a number, since it is also
/// Sainte.
const UNIT_MARKERS: &[&str] = &[
    "apt",
    "apartment",
    "app",
    "appt",
    "appartement",
    "unit",
    "suite",
    "ste",
    "bureau",
    "bur",
    "local",
    "room",
    "rm",
    "lot",
];

/// Splits on spaces and commas; a comma is kept as a mark on the word before it.
pub fn words(text: &str) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for piece in text.split_inclusive([',', ';']) {
        let comma = piece.ends_with([',', ';']);
        let piece = piece.trim_end_matches([',', ';']);
        let n = out.len();
        out.extend(piece.split_whitespace().map(|w| Word {
            text: w.to_owned(),
            comma: false,
        }));
        if comma && out.len() > n {
            out.last_mut().unwrap().comma = true;
        }
    }
    out
}

fn is_number_like(w: &str) -> bool {
    w.starts_with(|c: char| c.is_ascii_digit())
}

fn bare(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// "4e" or "1st" followed by a street type word.
fn ordinal_street(first: &str, next: &str) -> bool {
    let lower = first.to_lowercase();
    let digits = lower.bytes().take_while(u8::is_ascii_digit).count();
    let ordinal = matches!(
        &lower[digits..],
        "e" | "er" | "re" | "eme" | "ème" | "ieme" | "ième" | "st" | "nd" | "rd" | "th"
    );
    let typ = normalize::street_tokens(next)
        .first()
        .is_some_and(|t| normalize::is_generic(t) && !normalize::is_direction(t));
    ordinal && typ
}

/// Parses a query. `prefix` says the last word may be incomplete (typing); a trailing space
/// ends it.
pub fn parse(text: &str, prefix: bool) -> Parsed {
    let mut w = words(text);
    let mut p = Parsed {
        prefix: prefix && !text.ends_with(char::is_whitespace) && !w.is_empty(),
        ..Parsed::default()
    };

    // A unit marker and its value, anywhere after the first word.
    let mut i = 1;
    while i < w.len() {
        let t = &w[i].text;
        if let Some(rest) = t.strip_prefix('#').filter(|r| !r.is_empty()) {
            p.unit = Some(rest.to_owned());
            if w.remove(i).comma {
                w[i - 1].comma = true;
            }
            continue;
        }
        let marker = bare(t);
        let next = w.get(i + 1);
        let value_next = next.is_some_and(|n| {
            is_number_like(&n.text) || n.text.chars().count() <= 2 || n.text.starts_with('#')
        });
        let unit_marker = t == "#"
            || UNIT_MARKERS.contains(&marker.as_str())
                && value_next
                && (marker != "ste" || next.is_some_and(|n| is_number_like(&n.text)));
        if unit_marker && let Some(v) = next {
            let value = v
                .text
                .trim_start_matches('#')
                .trim_end_matches('.')
                .to_owned();
            let comma = v.comma;
            p.unit = Some(value);
            w.drain(i..i + 2);
            if comma {
                w[i - 1].comma = true;
            }
            continue;
        }
        i += 1;
    }

    // A postal code after the first word: "45201", "45201-1234", "K7L 3N6", "K7L3N6".
    if w.len() >= 2 {
        let n = w.len();
        let joined = format!("{}{}", w[n - 2].text, w[n - 1].text);
        let canadian2 = n >= 3
            && w[n - 2].text.len() == 3
            && normalize::postcode(&joined).is_some_and(|c| c.contains(' '));
        if canadian2 {
            p.postcode = normalize::postcode(&joined);
            w.truncate(n - 2);
        } else if let Some(code) = normalize::postcode(&w[n - 1].text) {
            // A Canadian code always; five digits after two words or a house number, so
            // "Route 66" and "Highway 40101" keep their numbers.
            let canadian = code.contains(' ');
            let us = w[n - 1].text.len() >= 5 && (n >= 3 || is_number_like(&w[0].text));
            if canadian || us {
                p.postcode = Some(code);
                w.truncate(n - 1);
            }
        }
        if p.postcode.is_some() {
            p.prefix = false;
            if let Some(last) = w.last_mut() {
                last.comma = true;
            }
        }
    } else if w.len() == 1 && normalize::postcode(&w[0].text).is_some_and(|c| c.contains(' ')) {
        // A Canadian code typed as one word stays a word, in token form.
        w[0].text = normalize::postcode_token(&normalize::postcode(&w[0].text).unwrap());
    }
    // A Canadian code typed as two words, alone: one word.
    if w.len() == 2
        && p.postcode.is_none()
        && w[0].text.len() == 3
        && let Some(code) = normalize::postcode(&format!("{}{}", w[0].text, w[1].text))
        && code.contains(' ')
    {
        w = vec![Word {
            text: normalize::postcode_token(&code),
            comma: false,
        }];
    }

    // A leading house number, with "1/2" as the next word, or a Canadian "unit-number";
    // not an ordinal before a street type ("4e Rue", "12e Avenue").
    if w.len() >= 2 && is_number_like(&w[0].text) && !ordinal_street(&w[0].text, &w[1].text) {
        let first = w[0].text.trim_end_matches(',').to_owned();
        let mut used = 1;
        let mut text = first.clone();
        if w[1].text.contains('/') && w.len() >= 3 && !w[0].comma {
            text = format!("{first} {}", w[1].text);
            used = 2;
        }
        if let Some(h) = normalize::house_number(&text) {
            if h.kind == NumberKind::Range
                && let Some(n) = h
                    .suffix
                    .strip_prefix('-')
                    .and_then(|s| s.parse::<u32>().ok())
            {
                p.numbers.push(Number {
                    number: HouseNumber {
                        number: n,
                        suffix: String::new(),
                        kind: NumberKind::Plain,
                    },
                    unit: Some(h.number.to_string()),
                });
            }
            p.numbers.push(Number {
                number: h,
                unit: None,
            });
            // A Range like "123-125" may also be just its first number.
            if p.numbers.len() == 2 {
                let n = p.numbers[1].number.number;
                p.numbers.push(Number {
                    number: HouseNumber {
                        number: n,
                        suffix: String::new(),
                        kind: NumberKind::Plain,
                    },
                    unit: None,
                });
            }
            // "123, rue Principale": the comma ends the number, not a part.
            w[used - 1].comma = false;
            p.lead = w.drain(..used).collect();
        }
    }
    p.words = w;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(p: &Parsed) -> Vec<&str> {
        p.words.iter().map(|w| w.text.as_str()).collect()
    }

    fn number(p: &Parsed) -> Option<(u32, &str, Option<&str>)> {
        p.numbers
            .first()
            .map(|n| (n.number.number, n.number.suffix.as_str(), n.unit.as_deref()))
    }

    #[test]
    fn splits_words_and_commas() {
        let w = words("123 Main St,  Springfield ,IL");
        let got: Vec<(&str, bool)> = w.iter().map(|w| (w.text.as_str(), w.comma)).collect();
        assert_eq!(
            got,
            [
                ("123", false),
                ("Main", false),
                ("St", true),
                ("Springfield", true),
                ("IL", false)
            ]
        );
    }

    #[test]
    fn parses_addresses() {
        let p = parse("123 Main St, Springfield, IL", false);
        assert_eq!(number(&p), Some((123, "", None)));
        assert_eq!(texts(&p), ["Main", "St", "Springfield", "IL"]);
        assert!(p.postcode.is_none() && p.unit.is_none());

        let p = parse("123A Main St Apt 4 Columbus OH 43215", false);
        assert_eq!(number(&p), Some((123, "A", None)));
        assert_eq!(p.unit.as_deref(), Some("4"));
        assert_eq!(p.postcode.as_deref(), Some("43215"));
        assert_eq!(texts(&p), ["Main", "St", "Columbus", "OH"]);

        let p = parse("123 1/2 Main St", false);
        assert_eq!(number(&p), Some((123, "1/2", None)));
        assert_eq!(texts(&p), ["Main", "St"]);

        let p = parse("4-123 Main St, Kingston ON K7L 3N6", false);
        assert_eq!(number(&p), Some((123, "", Some("4"))));
        assert_eq!(p.numbers[1].number.suffix, "-123");
        assert_eq!(p.postcode.as_deref(), Some("K7L 3N6"));
        assert_eq!(texts(&p), ["Main", "St", "Kingston", "ON"]);

        let p = parse("1200, rue Sainte-Catherine Ouest #300, Montréal", false);
        assert_eq!(number(&p), Some((1200, "", None)));
        assert_eq!(p.unit.as_deref(), Some("300"));
        assert_eq!(texts(&p), ["rue", "Sainte-Catherine", "Ouest", "Montréal"]);
        assert!(p.words[2].comma);

        let p = parse("5 Elm St # 4", false);
        assert_eq!(p.unit.as_deref(), Some("4"));
        assert_eq!(texts(&p), ["Elm", "St"]);

        let p = parse("12 Rue Ste-Anne", false);
        assert!(p.unit.is_none(), "Ste before a name is Sainte");

        let p = parse("4e Rue, Québec", false);
        assert!(p.numbers.is_empty(), "an ordinal street");
        assert_eq!(texts(&p), ["4e", "Rue", "Québec"]);
        assert_eq!(number(&parse("4E Main St", false)), Some((4, "E", None)));
    }

    #[test]
    fn keeps_numbers_that_name_things() {
        let p = parse("Route 66", false);
        assert!(p.numbers.is_empty() && p.postcode.is_none());
        assert_eq!(texts(&p), ["Route", "66"]);
        let p = parse("45201", false);
        assert!(p.numbers.is_empty() && p.postcode.is_none());
        assert_eq!(texts(&p), ["45201"]);
        let p = parse("K7L 3N6", true);
        assert_eq!(texts(&p), ["k7l3n6"]);
        let p = parse("100 Kingston", true);
        assert_eq!(number(&p), Some((100, "", None)));
        assert_eq!(texts(&p), ["Kingston"]);
        assert!(p.prefix);
        assert!(!parse("100 Kingston ", true).prefix);
    }
}
