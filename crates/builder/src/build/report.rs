//! What `build` prints: the stage times, and the counts and sizes of each part of the pack.

use std::time::Instant;

use omp_core::whitelist::Whitelist;
use rayon::prelude::*;

use crate::addr::{merge, store};
use crate::admin::Admin;
use crate::geom::{Geometry, P};
use crate::pbf::features;
use crate::search::{self, encode as search_encode};
use crate::tiles;
use crate::{mib, peak_rss_mib};

/// Prints each stage's time and the peak RSS so far.
pub struct Timer(Instant);

impl Timer {
    pub fn new() -> Self {
        Self(Instant::now())
    }

    pub fn stage(&mut self, name: &str) {
        eprintln!(
            "  {name}: {:.1} s, peak RSS {:.0} MiB",
            self.0.elapsed().as_secs_f64(),
            peak_rss_mib()
        );
        self.0 = Instant::now();
    }
}

/// Per-zoom totals printed at the end.
#[derive(Default, Clone, Copy)]
pub struct ZoomTotals {
    pub tiles: u64,
    pub frames: u64,
    pub raw: u64,
    pub bytes: u64,
}

/// The tiles, frames and bytes per zoom, and in all.
pub fn print_zooms(zooms: &[ZoomTotals]) {
    eprintln!("  zoom      tiles     frames       raw MiB    tiles MiB");
    let mut sum = ZoomTotals::default();
    for (z, zt) in zooms.iter().enumerate() {
        eprintln!(
            "  z{z:<3} {:>10} {:>10} {:>13.1} {:>12.1}",
            zt.tiles,
            zt.frames,
            mib(zt.raw),
            mib(zt.bytes)
        );
        sum.tiles += zt.tiles;
        sum.frames += zt.frames;
        sum.raw += zt.raw;
        sum.bytes += zt.bytes;
    }
    eprintln!(
        "  all  {:>10} {:>10} {:>13.1} {:>12.1}",
        sum.tiles,
        sum.frames,
        mib(sum.raw),
        mib(sum.bytes)
    );
}

/// Admin chains of places and POIs, per layer: `[features, in no area, without a level 4
/// area, without level 6, without level 8, in a city, town or village]`.
#[derive(Default)]
pub struct ChainStats {
    seen: std::collections::HashSet<(u8, i64)>,
    layers: std::collections::BTreeMap<String, [u64; 6]>,
}

impl ChainStats {
    /// Counts the point features of the `places` and `pois` layers, each once over all bands.
    pub fn add(&mut self, features: &[features::Feature], wl: &Whitelist, admin: &Admin) {
        let layers: Vec<u8> = ["places", "pois"]
            .iter()
            .filter_map(|l| wl.layer_index(l).map(|i| i as u8))
            .collect();
        let points: Vec<(u8, P)> = features
            .iter()
            .filter(|f| layers.contains(&f.layer) && self.seen.insert((f.layer, f.id)))
            .filter_map(|f| match f.geom {
                Geometry::Point(p) => Some((f.layer, p)),
                _ => None,
            })
            .collect();
        let counted: Vec<(u8, [u64; 6])> = points
            .par_iter()
            .map(|&(layer, p)| {
                let chain = admin.chain(p);
                let missing = |l| u64::from(admin.at_level(&chain, l).is_none());
                let town = chain.iter().any(|&a| admin.towns[a as usize]);
                (
                    layer,
                    [
                        1,
                        u64::from(chain.is_empty()),
                        missing(4),
                        missing(6),
                        missing(8),
                        u64::from(town),
                    ],
                )
            })
            .collect();
        for (layer, c) in counted {
            let row = self
                .layers
                .entry(wl.layers[layer as usize].name.clone())
                .or_default();
            for (r, v) in row.iter_mut().zip(c) {
                *r += v;
            }
        }
    }

    pub fn print(&self) {
        for (layer, c) in &self.layers {
            eprintln!(
                "  admin chains, {layer}: {} features, {} in no area, {} without level 4, \
                 {} without level 6, {} without level 8, {} in a city, town or village",
                c[0], c[1], c[2], c[3], c[4], c[5]
            );
        }
    }
}

/// Tiles left out (one polygon covering them), per zoom and per class.
pub fn print_left_out(cut: &tiles::Stats, wl: &Whitelist) {
    use std::collections::BTreeMap;
    let mut zooms: BTreeMap<u8, u64> = BTreeMap::new();
    let mut classes: BTreeMap<(u8, u16), (u64, u64)> = BTreeMap::new();
    for (&(z, l, c), &(t, b)) in &cut.left_out {
        *zooms.entry(z).or_default() += t;
        let e = classes.entry((l, c)).or_default();
        e.0 += t;
        e.1 += b;
    }
    let per_zoom: Vec<String> = zooms.iter().map(|(z, t)| format!("z{z} {t}")).collect();
    eprintln!(
        "  left out: {} tiles covered by one polygon ({})",
        zooms.values().sum::<u64>(),
        per_zoom.join(", ")
    );
    let mut classes: Vec<_> = classes.into_iter().collect();
    classes.sort_by_key(|c| std::cmp::Reverse(c.1.0));
    for ((l, c), (t, b)) in classes {
        let layer = &wl.layers[l as usize];
        eprintln!(
            "    {}/{}: {t} tiles, {:.1} MiB raw",
            layer.name,
            layer.classes[c as usize].name,
            mib(b)
        );
    }
}

/// Route relation coverage per country and class and per region, and the commonest networks.
pub fn print_routes(r: &features::RouteStats) {
    use std::collections::BTreeMap;
    // The last count is the ways with a real shield, out of all.
    let line = |name: &str, c: [u64; 4]| {
        let all: u64 = c[..3].iter().sum();
        let pct = |n: u64| 100.0 * n as f64 / all.max(1) as f64;
        eprintln!(
            "    {name:<20} {all:>9} {:>8.1}% {:>8.1}% {:>8.1}% {:>8.1}%",
            pct(c[0]),
            pct(c[1]),
            pct(c[2]),
            pct(c[3])
        );
    };
    let add = |t: &mut [u64; 4], c: &[u64; 4]| {
        for i in 0..4 {
            t[i] += c[i];
        }
    };
    eprintln!("  routes: {} road route relations", r.relations);
    eprintln!("    ways                      all  relation  ref only   neither    shield");
    // Country → class → counts; the class order is the whitelist's, roughly by importance.
    let order = ["motorway", "trunk", "primary", "secondary", "tertiary"];
    let mut countries: BTreeMap<&str, BTreeMap<usize, [u64; 4]>> = BTreeMap::new();
    for (region, classes) in &r.coverage {
        let country = region.split('-').next().unwrap_or(region);
        for (class, c) in classes {
            let i = order.iter().position(|o| o == class).unwrap_or(order.len());
            add(
                countries.entry(country).or_default().entry(i).or_default(),
                c,
            );
        }
    }
    for (country, classes) in &countries {
        let mut total = [0; 4];
        for (i, c) in classes {
            add(&mut total, c);
            line(
                &format!("{country} {}", order.get(*i).unwrap_or(&"other")),
                *c,
            );
        }
        line(&format!("{country} all"), total);
    }
    if r.coverage.len() > countries.len() {
        for (region, classes) in &r.coverage {
            let mut total = [0; 4];
            for c in classes.values() {
                add(&mut total, c);
            }
            line(region, total);
        }
    }
    let mut nets: Vec<(&String, &u64)> = r.networks.iter().collect();
    nets.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let top: Vec<String> = nets
        .iter()
        .take(40)
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
    eprintln!(
        "    {} networks; ways per network: {}",
        nets.len(),
        top.join(", ")
    );
}

pub fn print_search_sizes(s: &search_encode::Sizes) {
    let kinds: Vec<String> = s
        .raw_by_kind
        .iter()
        .map(|(k, b)| format!("{k:?} {:.2}", mib(*b)))
        .collect();
    eprintln!(
        "  search: {} KiB blocks: SENT {:.2} MiB ({:.2} raw: {}; street names {:.2}; {} \
         blocks), {:.2} MiB without street names; STWN {:.2} MiB, SFST {:.2} MiB, SPST {:.2} \
         MiB; {:.1} bytes per entry in all",
        s.block_kib,
        mib(s.sent),
        mib(s.sent_raw),
        kinds.join(", "),
        mib(s.street_names_raw),
        s.sent_blocks,
        mib(s.sent_without_street_names),
        mib(s.stwn),
        mib(s.sfst),
        mib(s.spst),
        (s.sent + s.stwn + s.sfst + s.spst) as f64 / s.entries.max(1) as f64
    );
}

pub fn print_address_sizes(s: &store::Sizes) {
    eprintln!(
        "  addresses: {} KiB blocks: {} on {} streets, {} unattached, {} ranges ({} dropped), \
         {} codes, {} exceptions; ASTR {:.2} MiB ({:.2} raw, {} blocks, {:.0} µs to decode \
         one), ASUF {:.2}, AUNI {:.2}, ALSE {:.2} ({} blocks), ARNG {:.2}, APCD {:.2}; {:.2} \
         MiB, {:.2} bytes per address",
        s.block_kib,
        s.addresses,
        s.streets_with_addresses,
        s.points,
        s.ranges,
        s.ranges_dropped,
        s.codes,
        s.exceptions,
        mib(s.astr),
        mib(s.astr_raw),
        s.astr_blocks,
        s.decode_us_per_block,
        mib(s.asuf),
        mib(s.auni),
        mib(s.alse),
        s.alse_blocks,
        mib(s.arng),
        mib(s.apcd),
        mib(s.total()),
        (s.total() - s.arng) as f64 / (s.addresses + s.points).max(1) as f64
    );
}

/// Totals of the per-subdivision merge lines.
pub fn print_address_rows(
    rows: &[merge::SubRow],
    streets: &[search::AddressStreet],
    postcodes: usize,
) {
    let sum = |f: &dyn Fn(&merge::SubRow) -> u64| rows.iter().map(f).sum::<u64>();
    let (records, addresses, attached) = (
        sum(&|r| r.records),
        sum(&|r| r.addresses),
        sum(&|r| r.attached),
    );
    let by: Vec<u64> = (0..4).map(|k| sum(&|r| r.by[k])).collect();
    eprintln!(
        "  addresses: {} subdivisions, {records} records ({} strict, {} loose, {} alias, {} \
         fuzzy, {} unattached) → {addresses} addresses, {attached} attached ({:.2}%), {} \
         approximate, {} units, {} of {} ranges kept; {} streets known only from {} \
         addresses, {postcodes} postcodes",
        rows.len(),
        by[0],
        by[1],
        by[2],
        by[3],
        sum(&|r| r.unattached_records),
        100.0 * attached as f64 / addresses.max(1) as f64,
        sum(&|r| r.approximate),
        sum(&|r| r.units),
        sum(&|r| r.fallback_ranges),
        sum(&|r| r.ranges),
        streets.len(),
        streets.iter().map(|s| s.addresses).sum::<u64>(),
    );
}
