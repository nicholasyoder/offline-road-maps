//! "What's here" (phase 2 steps 2.6 and 2.8): the nearest address, the nearest street, the
//! entries close by and the admin chain at a point.

use std::io;

use crate::admin::Area;
use crate::find::number_text;
use crate::geo::{self, P};
use crate::search::{Entry, Kind, NearAddress, Search};
use crate::street::{STEP_M, Spot, Street};

/// How far "what's here" looks for an address and a street (phase 2 plan, 2.8).
pub const ADDRESS_RADIUS_M: f64 = 100.0;

/// The nearest address to a point.
#[derive(Clone, Debug, PartialEq)]
pub struct HereAddress {
    /// "123 Main Street".
    pub label: String,
    pub p: P,
    pub dist_m: f64,
    /// Interpolated from a range, or an NAR blockface point.
    pub approx: bool,
    /// The street it is on, if it is attached.
    pub street: Option<u32>,
    pub postcode: Option<String>,
    pub units: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Here {
    pub p: P,
    pub address: Option<HereAddress>,
    pub street: Option<(u32, Street, Spot)>,
    /// Entries within the radius (places, POIs, labels), nearest first.
    pub nearby: Vec<(u32, Entry, f64)>,
    /// Admin areas holding the point, country first.
    pub chain: Vec<u32>,
}

impl Search {
    /// What is at `p`: the nearest address within `radius_m` (a point if there is one, else
    /// a number interpolated on the nearest street's range on that side), the nearest
    /// street within it, the entries within `entries_m` (none if 0), and the admin chain.
    pub fn here(&self, p: P, radius_m: f64, entries_m: f64) -> io::Result<Here> {
        let streets = self.streets_near(p, radius_m)?;
        let mut address = None;
        if let Some(n) = self.addresses_near(p, radius_m)?.into_iter().next() {
            let (label, street, approx, code, units) = match &n.address {
                NearAddress::Street(a) => (
                    format!(
                        "{} {}",
                        number_text(a.number, &a.suffix),
                        self.streets.street(a.street)?.name
                    ),
                    Some(a.street),
                    a.approx,
                    a.postcode,
                    a.units.clone(),
                ),
                NearAddress::Loose(l) => (
                    format!("{} {}", number_text(l.number, &l.suffix), l.street),
                    None,
                    l.approx,
                    l.postcode,
                    l.units.clone(),
                ),
            };
            let postcode = match (code, &self.addresses) {
                (Some(c), Some(a)) => Some(a.postcode(c)?),
                _ => None,
            };
            address = Some(HereAddress {
                label,
                p: n.p,
                dist_m: n.dist_m,
                approx,
                street,
                postcode,
                units,
            });
        }
        if address.is_none()
            && let (Some((id, spot)), Some(addresses)) = (streets.first(), &self.addresses)
        {
            let offset = spot.offset_m / STEP_M;
            let range = addresses.ranges(*id)?.into_iter().find(|r| {
                let (a, b) = (r.start.min(r.end), r.start.max(r.end));
                r.left == spot.left && f64::from(a) <= offset && offset <= f64::from(b)
            });
            if let Some(r) = range {
                let n = r.number_at(offset);
                address = Some(HereAddress {
                    label: format!("{n} {}", self.streets.street(*id)?.name),
                    p: spot.foot,
                    dist_m: spot.dist_m,
                    approx: true,
                    street: Some(*id),
                    postcode: None,
                    units: Vec::new(),
                });
            }
        }
        let street = match streets.first() {
            Some(&(id, spot)) => Some((id, self.streets.street(id)?, spot)),
            None => None,
        };
        let mpu = geo::metres_per_unit(p);
        let found = if entries_m > 0.0 {
            self.index.entries_in(&[geo::bbox_around(p, entries_m)])?
        } else {
            Vec::new()
        };
        let mut nearby: Vec<(u32, Entry, f64)> = found
            .into_iter()
            .map(|(id, e)| {
                let d = geo::dist(p, e.p) * mpu;
                (id, e, d)
            })
            .filter(|x| x.2 <= entries_m)
            .collect();
        nearby.sort_by(|a, b| a.2.total_cmp(&b.2));
        Ok(Here {
            p,
            address,
            street,
            nearby,
            chain: self.admin.chain(p)?,
        })
    }
}

/// What "what's here" shows, as the app's panel and `omp-builder here` give it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub p: P,
    pub address: Option<HereAddress>,
    /// The address's town, state and postcode: "New York, NY 10001".
    pub address_place: String,
    /// The nearest street: its name, town and distance.
    pub street: Option<(String, String, f64)>,
    /// Places, POIs and named features nearby, nearest first.
    pub nearby: Vec<(Entry, f64)>,
    /// The municipality, county, state and country holding the point, those it has.
    pub areas: Vec<String>,
    /// Every admin area holding the point, country first.
    pub chain: Vec<u32>,
}

impl Search {
    /// [`Search::here`] as shown: entries within `entries_m`, but only places, POIs and
    /// features, with names for the address's place and the areas.
    pub fn whats_here(&self, p: P, radius_m: f64, entries_m: f64) -> io::Result<Shown> {
        let h = self.here(p, radius_m, entries_m)?;
        let areas = self.admin.areas();
        let mut address_place = String::new();
        if let Some(a) = &h.address {
            // A loose address has no street record; the nearest street's town stands in.
            let town = match a.street {
                Some(id) => self.streets.street(id)?.town,
                None => h
                    .street
                    .as_ref()
                    .map_or(String::new(), |s| s.1.town.clone()),
            };
            let state = h
                .chain
                .iter()
                .map(|&i| &areas[i as usize])
                .find(|a| a.level == 4)
                .map(|s| if s.abbr.is_empty() { &s.name } else { &s.abbr });
            let mut parts: Vec<&str> = Vec::new();
            parts.extend(Some(town.as_str()).filter(|t| !t.is_empty()));
            parts.extend(state.map(String::as_str));
            address_place = parts.join(", ");
            if let Some(pc) = &a.postcode {
                if !address_place.is_empty() {
                    address_place.push(' ');
                }
                address_place += pc;
            }
        }
        Ok(Shown {
            p,
            address_place,
            street: h
                .street
                .as_ref()
                .map(|(_, st, spot)| (st.name.clone(), st.town.clone(), spot.dist_m)),
            nearby: h
                .nearby
                .into_iter()
                .filter(|(_, e, _)| matches!(e.kind, Kind::Place | Kind::Poi | Kind::Feature))
                .map(|(_, e, d)| (e, d))
                .collect(),
            areas: shown_areas(areas, &h.chain)
                .into_iter()
                .map(|a| a.name.clone())
                .collect(),
            address: h.address,
            chain: h.chain,
        })
    }
}

/// The areas "what's here" names, innermost first: the municipality (the deepest area at
/// level 7 or 8, a township too, whatever the town flag says), the county (level 6, else
/// 5), the state or province (4) and the country (2). Boroughs and neighbourhoods (9–10)
/// are left out.
pub fn shown_areas<'a>(areas: &'a [Area], chain: &[u32]) -> Vec<&'a Area> {
    let deepest = |levels: &[u8]| {
        levels.iter().find_map(|&l| {
            chain
                .iter()
                .rev()
                .map(|&i| &areas[i as usize])
                .find(|a| a.level == l)
        })
    };
    [
        deepest(&[8, 7]),
        deepest(&[6, 5]),
        deepest(&[4]),
        deepest(&[2]),
    ]
    .into_iter()
    .flatten()
    .collect()
}
