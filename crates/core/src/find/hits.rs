//! A candidate as shown: its name, context and radius, and whether two hits show the same
//! thing.

use std::io;

use super::candidates::half_diagonal_m;
use super::score::place_radius_m;
use super::{Hit, HitKind, Run, Scored};
use crate::geo;
use crate::search::{Entry, Kind};

pub(super) fn titlecase(s: &str) -> String {
    if s.chars().any(char::is_lowercase) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric() && c != '\'';
    }
    out
}

/// An entry's name as shown: names from address sources only ("THORNBIRD CRES") in mixed
/// case.
pub(super) fn shown_name(e: &Entry) -> String {
    match e.kind {
        Kind::AddressStreet => titlecase(&e.name),
        _ => e.name.clone(),
    }
}

/// Whether two hits show the same thing: same kind, name and context, close together.
/// A street's records (carriageways, pieces split at towns and at 20 km) are one, whatever
/// their towns, when they meet or nearly; `radius` gives a street hit's half length.
pub(super) fn same(
    a: &Hit,
    b: &Hit,
    radius: &mut impl FnMut(&Hit) -> io::Result<f64>,
) -> io::Result<bool> {
    if a.kind != b.kind || !a.name.eq_ignore_ascii_case(&b.name) {
        return Ok(false);
    }
    let (near_m, context) = match a.kind {
        HitKind::Address => (100.0, true),
        HitKind::Entry(Kind::Street) => (radius(a)? + radius(b)? + 1_000.0, a.number_missing),
        _ => (3_000.0, true),
    };
    Ok((!context || a.context == b.context)
        && geo::dist(a.p, b.p) * geo::metres_per_unit(a.p) <= near_m)
}

impl Run<'_> {
    fn context(&self, e: &Entry, chain: &[u32]) -> String {
        let areas = self.s.admin.areas();
        let mut parts: Vec<&str> = Vec::new();
        let town = e
            .town
            .and_then(|t| self.s.index.town(t))
            .filter(|t| !t.is_empty())
            .or_else(|| {
                chain
                    .iter()
                    .map(|&a| &areas[a as usize])
                    .find(|a| (7..=10).contains(&a.level) && a.name != e.name)
                    .map(|a| a.name.as_str())
            });
        if let Some(t) = town.filter(|t| *t != e.name) {
            parts.push(t);
        }
        let mut state = None;
        for &a in chain {
            let area = &areas[a as usize];
            if area.level == 4 && area.name != e.name {
                state = Some(area.name.as_str());
            }
        }
        match state {
            Some(s) => parts.push(s),
            None => {
                if let Some(c) = chain
                    .iter()
                    .map(|&a| &areas[a as usize])
                    .find(|a| a.level == 2 && a.name != e.name)
                {
                    parts.push(&c.name);
                }
            }
        }
        parts.join(", ")
    }

    pub(super) fn radius_m(&self, e: &Entry, chain: &[u32]) -> io::Result<f64> {
        Ok(match e.kind {
            Kind::Admin | Kind::Place => {
                match chain
                    .first()
                    .filter(|_| e.kind == Kind::Admin || e.reference > 0)
                {
                    Some(&a) => half_diagonal_m(self.s.admin.areas()[a as usize].bbox),
                    None => place_radius_m(e.importance) / 2.0,
                }
            }
            Kind::Street => self.s.street_radius_m(e.reference as u32)?,
            Kind::Postcode | Kind::AddressStreet => (e.reference as f64).max(300.0),
            Kind::Poi => 150.0,
            Kind::Feature => 2_000.0,
        })
    }

    pub(super) fn entry_hit(&self, c: &Scored) -> io::Result<Hit> {
        let e = &c.entry;
        Ok(Hit {
            kind: HitKind::Entry(e.kind),
            class: e.class,
            name: shown_name(e),
            context: self.context(e, &c.chain),
            p: e.p,
            // A street's needs its record: left to the hits shown (`Search::find`).
            radius_m: if e.kind == Kind::Street {
                f64::NAN
            } else {
                self.radius_m(e, &c.chain)?
            },
            approx: false,
            number_missing: false,
            postcode: None,
            units: Vec::new(),
            entry: Some(c.id),
            street: (e.kind == Kind::Street).then_some(e.reference as u32),
            score: c.score,
            exact: c.exact,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_names() {
        assert_eq!(titlecase("THORNBIRD CRES"), "Thornbird Cres");
        assert_eq!(titlecase("O'NEIL RD"), "O'neil Rd");
        assert_eq!(titlecase("McDonald Road"), "McDonald Road");
    }
}
