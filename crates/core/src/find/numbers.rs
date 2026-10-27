//! House numbers resolved to addresses: a point on the street, its range, or an unattached
//! point of the street's name nearby.

use std::io;

use super::hits::{shown_name, titlecase};
use super::{Hit, HitKind, Run, Scored};
use crate::addr::Loose;
use crate::geo::P;
use crate::normalize::{loose_key, street_key, tokens};
use crate::query::Number;
use crate::search::{self, Found, Kind};

/// How far below the best a candidate for a house number may score.
const LOCATE_WITHIN: f64 = 15.0;

/// A house number as shown: "123A", "123 1/2", "123-125", "12 bis".
pub fn number_text(number: u32, suffix: &str) -> String {
    match suffix {
        "" => number.to_string(),
        s if s.starts_with('-') => format!("{number}{s}"),
        s if s.contains('/') => format!("{number} {s}"),
        s if s.len() == 1 => format!("{number}{s}"),
        s => format!("{number} {}", s.to_lowercase()),
    }
}

impl Run<'_> {
    /// Unattached points numbered `number` within `radius_m` of `p` whose street name has
    /// the key of `name`: for a street known only from addresses (kind 6), or one whose
    /// addresses spell it differently enough not to attach ("ch. Arnold").
    fn loose_points(&self, name: &str, p: P, radius_m: f64, number: u32) -> io::Result<Vec<Loose>> {
        let Some(addresses) = &self.s.addresses else {
            return Ok(Vec::new());
        };
        let key = loose_key(&street_key(name));
        let mut out = Vec::new();
        for ((x, y), c) in self.s.grid.around(p, radius_m.min(5_000.0))? {
            if !c.loose {
                continue;
            }
            for l in addresses.loose_in(search::loose_cell(x, y))? {
                if l.number == number && loose_key(&street_key(&l.street)) == key {
                    out.push(l);
                }
            }
        }
        Ok(out)
    }

    /// The address hits of a reading with a house number, from its street candidates.
    pub(super) fn addresses(
        &mut self,
        number: &Number,
        unit: Option<&str>,
        postcode: Option<&str>,
        cands: &[Scored],
    ) -> io::Result<Vec<Hit>> {
        let n = &number.number;
        let unit = number.unit.as_deref().or(unit);
        let unit_tok = unit.map(|u| tokens(u).join(" "));
        let mut out = Vec::new();
        // The streets that match about as well as the best: the number decides among them.
        let best = cands.first().map_or(0.0, |c| c.score);
        for c in cands
            .iter()
            .take(self.b.locate)
            .take_while(|c| c.score >= best - LOCATE_WITHIN)
        {
            self.check()?;
            let e = &c.entry;
            let mut base = self.entry_hit(c)?;
            base.name = format!("{} {}", number_text(n.number, &n.suffix), base.name);
            base.kind = HitKind::Address;
            base.radius_m = 50.0;
            base.entry = Some(c.id);
            let loose = |run: &Self, radius_m: f64| -> io::Result<_> {
                let mut pts = run.loose_points(&e.name, e.p, radius_m, n.number)?;
                pts.sort_by_key(|l| (l.suffix != n.suffix, l.approx, !l.osm));
                Ok(pts
                    .into_iter()
                    .next()
                    .map(|l| (l.p, l.approx, l.suffix, l.postcode, l.units, false)))
            };
            let found = match e.kind {
                Kind::Street => {
                    let mut located = self.s.locate(e.reference as u32, n.number, &n.suffix)?;
                    // Of several points with the number, an exact one, then OSM's.
                    located.sort_by_key(|l| match &l.found {
                        Found::Point(a) => (a.suffix != n.suffix, l.approx, !a.osm),
                        Found::Range(_) => (true, true, true),
                    });
                    match located.into_iter().next() {
                        Some(l) => Some(match l.found {
                            Found::Point(a) => {
                                (l.p, l.approx, a.suffix, a.postcode, a.units, false)
                            }
                            Found::Range(_) => (l.p, true, String::new(), None, Vec::new(), true),
                        }),
                        None => loose(self, self.radius_m(e, &c.chain)? + 300.0)?,
                    }
                }
                Kind::AddressStreet => loose(self, e.reference as f64 * 2.0 + 300.0)?,
                _ => None,
            };
            let mut hit = base;
            match found {
                Some((p, approx, suffix, code, units, range)) => {
                    hit.p = p;
                    hit.approx = approx;
                    hit.name = format!("{} {}", number_text(n.number, &suffix), shown_name(e));
                    hit.score += if range { 25.0 } else { 30.0 };
                    if suffix != n.suffix {
                        hit.score -= 3.0;
                        hit.exact = false;
                    }
                    if let (Some(code), Some(a)) = (code, &self.s.addresses) {
                        hit.postcode = Some(a.postcode(code)?);
                    }
                    if let Some(q) = postcode {
                        if hit.postcode.as_deref() == Some(q) {
                            hit.score += 5.0;
                        } else if hit.postcode.is_some() {
                            hit.score -= 5.0;
                        }
                    }
                    if let Some(u) = &unit_tok
                        && units.iter().any(|x| tokens(x).ends_with(&tokens(u)))
                    {
                        hit.score += 3.0;
                    }
                    hit.units = units.iter().map(|u| titlecase(u)).collect();
                }
                None => {
                    hit = self.entry_hit(c)?;
                    hit.number_missing = true;
                    hit.exact = false;
                    hit.score -= 10.0;
                }
            }
            out.push(hit);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_numbers() {
        assert_eq!(number_text(123, ""), "123");
        assert_eq!(number_text(123, "A"), "123A");
        assert_eq!(number_text(123, "1/2"), "123 1/2");
        assert_eq!(number_text(123, "-125"), "123-125");
        assert_eq!(number_text(12, "BIS"), "12 bis");
    }
}
