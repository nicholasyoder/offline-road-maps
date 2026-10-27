//! `omp-builder report`: turns `measure` JSON files into Markdown tables.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

#[derive(Args)]
pub struct ReportArgs {
    /// JSON files written by `measure`, one per region (or per band).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Print only the headline size table: one column per `measure` region, with totals.
    #[arg(long)]
    size_table: bool,

    /// With `--size-table`: extrapolate a planet column from REGION by the planet PBF's size
    /// over REGION's.
    #[arg(long, value_name = "REGION=BYTES", requires = "size_table")]
    planet: Option<String>,

    /// With `--size-table`: CoMaps `countries.txt`, to compare with its `.mwm` sizes.
    #[arg(long, requires = "size_table")]
    comaps: Option<PathBuf>,

    /// With `--comaps`: the CoMaps groups or maps that cover a region (`planet` for the
    /// planet column); a leading `-` subtracts one, e.g. `quebec=Canada_Quebec`.
    #[arg(long, value_name = "REGION=ID,ID,-ID", requires = "comaps")]
    comaps_ids: Vec<String>,
}

const VARIANTS: [&str; 6] = ["v0", "v1", "v2", "v3", "v4", "v5"];
/// The variant used for the per-layer and per-zoom tables.
const MAIN: &str = "v4";

fn mib_text(b: f64) -> String {
    let m = (b / f64::from(1 << 20)).max(0.0);
    if m >= 100.0 {
        format!("{m:.0}")
    } else {
        format!("{m:.1}")
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

pub fn run(args: ReportArgs) -> Result<()> {
    let mut runs = Vec::new();
    for p in &args.inputs {
        let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
        let v: Value =
            serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        runs.push(v);
    }
    anyhow::ensure!(
        runs.iter().all(|r| r["kind"] != "addr"),
        "addr-spike runs are no longer reported (the spike was retired in refactor step R3)"
    );
    let runs = merge_bands(runs)?;
    if args.size_table {
        print!("{}", size_table(&runs, &args)?);
        return Ok(());
    }
    let names: Vec<String> = runs
        .iter()
        .map(|r| {
            let s = r["node_strategy"].as_str().unwrap_or("?");
            format!("{} ({s})", r["region"].as_str().unwrap_or("?"))
        })
        .collect();
    // (variant, layer, zoom) → bytes, per run.
    let cells: Vec<BTreeMap<(String, String, u64), f64>> = runs
        .iter()
        .map(|r| {
            r["cells"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    let key = (
                        c["variant"].as_str().unwrap_or_default().to_owned(),
                        c["layer"].as_str().unwrap_or_default().to_owned(),
                        c["zoom"].as_u64().unwrap_or_default(),
                    );
                    (key, num(&c["bytes"]))
                })
                .collect()
        })
        .collect();
    let mut out = String::new();
    let header = |out: &mut String, first: &str| {
        let _ = write!(out, "| {first} |");
        for n in &names {
            let _ = write!(out, " {n} |");
        }
        let _ = write!(out, "\n|---|");
        for _ in &names {
            out.push_str("---:|");
        }
        out.push('\n');
    };

    let _ = writeln!(out, "## Build\n");
    header(&mut out, "");
    let row = |out: &mut String, label: &str, f: &dyn Fn(&Value) -> String| {
        let _ = write!(out, "| {label} |");
        for r in &runs {
            let _ = write!(out, " {} |", f(r));
        }
        out.push('\n');
    };
    row(&mut out, "Input PBF (MiB)", &|r| {
        mib_text(num(&r["input_bytes"]))
    });
    row(&mut out, "Wall time (s)", &|r| {
        format!("{:.0}", num(&r["build"]["wall_s"]))
    });
    row(&mut out, "Peak RSS (MiB)", &|r| {
        format!("{:.0}", num(&r["build"]["peak_rss_mib"]))
    });
    for stage in ["read", "routing", "tiles", "compress"] {
        row(&mut out, &format!("  {stage} (s)"), &|r| {
            r["build"]["stages"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["name"] == stage)
                .map_or("–".into(), |s| format!("{:.0}", num(&s["secs"])))
        });
    }
    row(&mut out, "Node index on disk (MiB)", &|r| {
        mib_text(num(&r["build"]["node_index_disk_bytes"]))
    });
    row(&mut out, "Tile store temp (MiB)", &|r| {
        mib_text(num(&r["build"]["tile_store_bytes"]))
    });

    let _ = writeln!(out, "\n## Tiles z0–z14 by variant (MiB)\n");
    header(&mut out, "Variant");
    for v in VARIANTS {
        let _ = write!(out, "| {} |", v.to_uppercase());
        for c in &cells {
            let total: f64 = c.iter().filter(|(k, _)| k.0 == v).map(|(_, b)| b).sum();
            let _ = write!(out, " {} |", mib_text(total));
        }
        out.push('\n');
    }
    let _ = write!(out, "| V4 dictionaries |");
    for r in &runs {
        let d: f64 = r["dictionaries"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| num(&d["dict_bytes"]))
            .sum();
        let _ = write!(out, " {} |", mib_text(d));
    }
    let _ = write!(out, "\n| MVT + gzip (V2 geometry) |");
    for r in &runs {
        let gz: f64 = r["mvt"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| num(&m["gzip_bytes"]))
            .sum();
        let _ = write!(out, " {} |", mib_text(gz));
    }
    out.push('\n');

    let mut layers: Vec<String> = cells
        .iter()
        .flat_map(|c| c.keys().map(|k| k.1.clone()))
        .collect();
    layers.sort();
    layers.dedup();
    let _ = writeln!(out, "\n## {} by layer (MiB, z0–z14)\n", MAIN.to_uppercase());
    header(&mut out, "Layer");
    for l in &layers {
        let _ = write!(out, "| {l} |");
        for c in &cells {
            let total: f64 = c
                .iter()
                .filter(|(k, _)| k.0 == MAIN && &k.1 == l)
                .map(|(_, b)| b)
                .sum();
            let _ = write!(out, " {} |", mib_text(total));
        }
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "\n## {} by zoom (MiB, all layers)\n",
        MAIN.to_uppercase()
    );
    header(&mut out, "Zoom");
    for z in 0..=14u64 {
        let _ = write!(out, "| {z} |");
        for c in &cells {
            let total: f64 = c
                .iter()
                .filter(|(k, _)| k.0 == MAIN && k.2 == z)
                .map(|(_, b)| b)
                .sum();
            let _ = write!(out, " {} |", mib_text(total));
        }
        out.push('\n');
    }

    let _ = writeln!(out, "\n## Routing and search\n");
    header(&mut out, "");
    row(&mut out, "Graph nodes", &|r| {
        format!("{:.0}", num(&r["routing"]["nodes"]))
    });
    row(&mut out, "Graph edges", &|r| {
        format!("{:.0}", num(&r["routing"]["edges"]))
    });
    let ratio = ch_ratio(&runs);
    row(&mut out, "CH prepare (s)", &|r| {
        if has_ch(r) {
            format!("{:.0}", num(&r["routing"]["ch_prepare_s"]))
        } else {
            "–".into()
        }
    });
    row(
        &mut out,
        "CH, compact layout (MiB)",
        &|r| match ch_bytes(r, ratio) {
            Some((b, false)) => mib_text(b),
            Some((b, true)) => format!(
                "~{} (est., {:.2} CH edges per edge)",
                mib_text(b),
                ratio.unwrap_or_default()
            ),
            None => "–".into(),
        },
    );
    row(&mut out, "Search fst (MiB)", &|r| {
        mib_text(num(&r["search"]["fst_bytes"]))
    });
    row(&mut out, "Search postings (MiB)", &|r| {
        mib_text(num(&r["search"]["postings_bytes"]))
    });
    row(&mut out, "Search entries (MiB)", &|r| {
        mib_text(num(&r["search"]["entries_bytes"]))
    });
    print!("{out}");
    Ok(())
}

fn has_ch(r: &Value) -> bool {
    r["routing"]["ch_edges"].is_number()
}

/// The mean CH-edges-per-edge ratio of the runs that built a CH.
fn ch_ratio(runs: &[Value]) -> Option<f64> {
    let ratios: Vec<f64> = runs
        .iter()
        .filter(|r| has_ch(r))
        .map(|r| num(&r["routing"]["ch_edges"]) / num(&r["routing"]["edges"]).max(1.0))
        .collect();
    (!ratios.is_empty()).then(|| ratios.iter().sum::<f64>() / ratios.len() as f64)
}

/// CH bytes in the compact layout, and whether they are estimated: runs that only counted
/// the graph (`--routing graph`) are estimated from `ratio` (12 bytes per node and per edge).
fn ch_bytes(r: &Value, ratio: Option<f64>) -> Option<(f64, bool)> {
    let g = &r["routing"];
    match ratio {
        _ if has_ch(r) => Some((num(&g["ch_bytes_compact"]), false)),
        Some(k) if g["edges"].is_number() => {
            Some(((num(&g["edges"]) * k + num(&g["nodes"])) * 12.0, true))
        }
        _ => None,
    }
}

/// The headline sizes of one region, in bytes (graph sizes in counts).
#[derive(Clone, Default)]
struct Sizes {
    tiles: f64,
    water: f64,
    landcover: f64,
    roads: f64,
    tile_index: f64,
    nodes: f64,
    edges: f64,
    ch: Option<f64>,
    ch_estimated: bool,
    search: f64,
}

impl Sizes {
    fn total(&self) -> f64 {
        self.tiles + self.tile_index + self.ch.unwrap_or(0.0) + self.search
    }

    fn scaled(&self, k: f64) -> Self {
        let m = |v: Option<f64>| v.map(|v| v * k);
        Self {
            tiles: self.tiles * k,
            water: self.water * k,
            landcover: self.landcover * k,
            roads: self.roads * k,
            tile_index: self.tile_index * k,
            nodes: self.nodes * k,
            edges: self.edges * k,
            ch: m(self.ch),
            ch_estimated: true,
            search: self.search * k,
        }
    }
}

/// Splits `REGION=VALUE`.
fn region_arg(s: &str) -> Result<(&str, &str)> {
    s.split_once('=')
        .with_context(|| format!("expected REGION=VALUE, got {s:?}"))
}

/// Sums the `.mwm` sizes under `ids` in CoMaps' `countries.txt` tree (`-ID` subtracts).
fn comaps_bytes(tree: &Value, ids: &str) -> Result<f64> {
    fn total(n: &Value) -> f64 {
        num(&n["s"])
            + n["g"]
                .as_array()
                .into_iter()
                .flatten()
                .map(total)
                .sum::<f64>()
    }
    fn find<'a>(n: &'a Value, id: &str) -> Option<&'a Value> {
        if n["id"] == id {
            return Some(n);
        }
        n["g"].as_array()?.iter().find_map(|c| find(c, id))
    }
    let mut sum = 0.0;
    for id in ids.split(',') {
        let (sign, id) = id.strip_prefix('-').map_or((1.0, id), |i| (-1.0, i));
        let n = find(tree, id).with_context(|| format!("CoMaps id {id:?} not found"))?;
        sum += sign * total(n);
    }
    Ok(sum)
}

/// The headline table: tiles, routing and search per region, their total, a
/// planet extrapolation and the CoMaps maps that cover the same ground.
fn size_table(runs: &[Value], args: &ReportArgs) -> Result<String> {
    let region = |r: &Value| r["region"].as_str().unwrap_or("?").to_owned();
    let ratio = ch_ratio(runs);
    let mut cols: Vec<(String, Sizes)> = runs
        .iter()
        .map(|r| {
            let layer = |l: &str| {
                r["cells"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["variant"] == MAIN && (l.is_empty() || c["layer"] == l))
                    .map(|c| num(&c["bytes"]))
                    .sum::<f64>()
            };
            let s = &r["search"];
            let ch = ch_bytes(r, ratio);
            let sizes = Sizes {
                tiles: layer(""),
                water: layer("water"),
                landcover: layer("landcover"),
                roads: layer("roads"),
                tile_index: r["tile_index"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|z| num(&z["sparse_bytes"]))
                    .sum(),
                nodes: num(&r["routing"]["nodes"]),
                edges: num(&r["routing"]["edges"]),
                ch: ch.map(|c| c.0),
                ch_estimated: ch.is_some_and(|c| c.1),
                search: num(&s["fst_bytes"]) + num(&s["postings_bytes"]) + num(&s["entries_bytes"]),
            };
            (region(r), sizes)
        })
        .collect();
    if let Some(p) = &args.planet {
        let (name, bytes) = region_arg(p)?;
        let bytes: f64 = bytes.parse().context("--planet: bytes")?;
        let base = runs
            .iter()
            .position(|r| region(r) == name)
            .with_context(|| format!("--planet {name}: no such region"))?;
        let k = bytes / num(&runs[base]["input_bytes"]).max(1.0);
        cols.push(("planet".into(), cols[base].1.scaled(k)));
    }
    let comaps: Option<Vec<Option<f64>>> = match &args.comaps {
        None => None,
        Some(p) => {
            let text =
                std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            let tree: Value = serde_json::from_str(&text).context("parse countries.txt")?;
            let mut ids = BTreeMap::new();
            for a in &args.comaps_ids {
                let (name, list) = region_arg(a)?;
                ids.insert(name, list);
            }
            let sizes = cols
                .iter()
                .map(|(name, _)| ids.get(name.as_str()).map(|i| comaps_bytes(&tree, i)))
                .map(Option::transpose)
                .collect::<Result<_>>()?;
            Some(sizes)
        }
    };

    let mut out = String::new();
    let _ = write!(out, "| Layer |");
    for (name, _) in &cols {
        let name = if name == "planet" {
            "planet (extrapolated)"
        } else {
            name
        };
        let _ = write!(out, " {name} |");
    }
    let _ = write!(out, "\n|---|");
    for _ in &cols {
        out.push_str("---:|");
    }
    out.push('\n');
    let dash = || "–".to_owned();
    let big = |b: f64| {
        let m = b / f64::from(1 << 20);
        if m >= 1000.0 {
            count(&Value::from(m.round() as u64))
        } else {
            mib_text(b)
        }
    };
    let millions = |n: f64| format!("{:.1}M", n / 1e6);
    let mut row = |label: &str, f: &dyn Fn(&Sizes) -> String| {
        let _ = write!(out, "| {label} |");
        for (_, s) in &cols {
            let _ = write!(out, " {} |", f(s));
        }
        out.push('\n');
    };
    row(&format!("Tiles {} (MiB)", MAIN.to_uppercase()), &|s| {
        big(s.tiles)
    });
    row("— water / landcover / roads", &|s| {
        format!("{} / {} / {}", big(s.water), big(s.landcover), big(s.roads))
    });
    row("— other layers", &|s| {
        big(s.tiles - s.water - s.landcover - s.roads)
    });
    row("Tile index, sparse (MiB)", &|s| big(s.tile_index));
    row("Routing graph nodes / edges", &|s| {
        format!("{} / {}", millions(s.nodes), millions(s.edges))
    });
    row("CH, compact layout (MiB)", &|s| {
        s.ch.map_or_else(dash, |c| {
            format!("{}{}", if s.ch_estimated { "~" } else { "" }, big(c))
        })
    });
    row("Search: fst + postings + entries (MiB)", &|s| big(s.search));
    row("**Total (MiB)**", &|s| format!("**{}**", big(s.total())));
    row("Total (GiB / GB)", &|s| {
        format!(
            "{:.1} / {:.1}",
            s.total() / f64::from(1 << 30),
            s.total() / 1e9
        )
    });
    if let Some(comaps) = &comaps {
        let _ = write!(out, "| CoMaps `.mwm` files (MiB) |");
        for c in comaps {
            let _ = write!(out, " {} |", c.map_or_else(dash, big));
        }
        let _ = write!(out, "\n| Total / CoMaps |");
        for (c, (_, s)) in comaps.iter().zip(&cols) {
            let _ = write!(
                out,
                " {} |",
                c.map_or_else(dash, |c| format!("{:.2}", s.total() / c))
            );
        }
        out.push('\n');
    }
    if let Some(k) = ratio {
        let _ = writeln!(out, "\n~ CH estimated at {k:.2} CH edges per graph edge.");
    }
    Ok(out)
}

/// 1234567 → "1,234,567".
fn count(v: &Value) -> String {
    let n = v.as_u64().unwrap_or(0).to_string();
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && (n.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Fields that identify a row in a list; rows with the same values are summed together.
const ROW_KEYS: [&str; 9] = [
    "source",
    "license",
    "variant",
    "dedup_m",
    "block_kib",
    "path",
    "name",
    "layer",
    "zoom",
];
/// Settings, kept from the first run rather than summed.
const SETTINGS: [&str; 3] = ["attach_m", "dedup_m", "zstd_level"];

/// Adds `b` into `a`: numbers add (peak RSS takes the maximum; row keys and settings keep
/// `a`'s value), lists of rows are matched by [`ROW_KEYS`], number lists add element-wise,
/// and strings keep `a`'s value.
fn add_value(a: &mut Value, b: &Value, key: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y))
            if !SETTINGS.contains(&key) && !ROW_KEYS.contains(&key) =>
        {
            let (x0, y0) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            let v = if key == "peak_rss_mib" {
                Value::from(x0.max(y0))
            } else if let (Some(i), Some(j)) = (x.as_u64(), y.as_u64()) {
                Value::from(i + j)
            } else {
                Value::from(x0 + y0)
            };
            *x = v.as_number().cloned().unwrap_or_else(|| x.clone());
        }
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in y {
                match x.get_mut(k) {
                    Some(old) => add_value(old, v, k),
                    None => {
                        x.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            let row_key = |v: &Value| -> Option<Vec<Value>> {
                v.is_object()
                    .then(|| ROW_KEYS.iter().map(|k| v[*k].clone()).collect())
            };
            if y.first().is_some_and(Value::is_number) && x.len() == y.len() {
                for (old, v) in x.iter_mut().zip(y) {
                    add_value(old, v, key);
                }
            } else if y.first().is_some_and(Value::is_object) {
                for v in y {
                    let k = row_key(v);
                    match x.iter_mut().find(|old| row_key(old) == k) {
                        Some(old) => add_value(old, v, key),
                        None => x.push(v.clone()),
                    }
                }
            }
        }
        (a @ Value::Null, b) => *a = b.clone(),
        _ => {}
    }
}

/// Adds up `measure --band` runs of one region into one run. Tile bytes, MVT, the tile
/// index and per-class z14 bytes add exactly, since each tile is cut in one band, and so do
/// per-class feature totals, since each feature is owned by one band. Search sizes add up to
/// an upper bound (the bands' token sets overlap). Element and tag counts, routing,
/// dictionaries and read benchmarks are band 0's, which sees the whole input too.
fn merge_bands(runs: Vec<Value>) -> Result<Vec<Value>> {
    let band = |r: &Value| r["band"][0].as_u64();
    let (mut banded, mut out): (Vec<Value>, Vec<Value>) =
        runs.into_iter().partition(|r| band(r).is_some());
    banded.sort_by_key(|r| (r["region"].to_string(), band(r)));
    for r in banded {
        let n = r["band"][1].as_u64().unwrap_or(1);
        let base = out
            .iter_mut()
            .find(|o| o["region"] == r["region"] && o["bands"].is_array());
        let Some(base) = base else {
            anyhow::ensure!(
                band(&r) == Some(0),
                "{}: band runs must start at band 0",
                r["region"]
            );
            let mut r = r;
            r["bands"] = Value::from(vec![Value::from(0), Value::from(n)]);
            out.push(r);
            continue;
        };
        let seen = base["bands"][0].as_u64().unwrap_or(0) + 1;
        anyhow::ensure!(
            band(&r) == Some(seen) && base["bands"][1].as_u64() == Some(n),
            "{}: expected band {seen}/{n}",
            r["region"]
        );
        base["bands"][0] = Value::from(seen);
        add_value(&mut base["build"], &r["build"], "build");
        for key in ["cells", "mvt", "tile_index", "search"] {
            add_value(&mut base[key], &r[key], key);
        }
        if let Some(p) = r["water"]["polygons"].as_u64() {
            base["water"]["polygons"] =
                Value::from(base["water"]["polygons"].as_u64().unwrap_or(0) + p);
        }
        let classes = base["classes"].as_array_mut().into_iter().flatten();
        for c in classes {
            let other = r["classes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["layer"] == c["layer"] && o["class"] == c["class"]);
            if let Some(o) = other {
                for k in ["features", "vertices", "z14_features", "z14_v1_bytes"] {
                    c[k] = Value::from(c[k].as_u64().unwrap_or(0) + o[k].as_u64().unwrap_or(0));
                }
            }
        }
    }
    for r in &mut out {
        if let Some([k, n]) = r["bands"].as_array().map(|b| [num(&b[0]), num(&b[1])]) {
            anyhow::ensure!(
                k + 1.0 == n,
                "{}: only {} of {n} bands",
                r["region"],
                k + 1.0
            );
            r["bands"] = Value::from(n as u64);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_bands() {
        let band = |k: u64, z14: u64| {
            serde_json::json!({
                "region": "r", "band": [k, 2], "read": {"nodes": 10},
                "build": {"wall_s": 1.0, "peak_rss_mib": 5.0 + k as f64},
                "cells": [{"variant": "v4", "layer": "roads", "zoom": 14, "bytes": z14}],
                "classes": [{"layer": "roads", "class": "primary", "ways": 3, "features": 2,
                             "vertices": 4, "z14_features": 1, "z14_v1_bytes": z14}]
            })
        };
        let merged = merge_bands(vec![band(1, 7), band(0, 5)]).unwrap();
        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m["bands"], 2);
        assert_eq!(m["cells"][0]["zoom"], 14);
        assert_eq!(m["cells"][0]["bytes"], 12);
        assert_eq!(m["classes"][0]["z14_v1_bytes"], 12);
        assert_eq!(m["classes"][0]["ways"], 3);
        assert_eq!(m["read"]["nodes"], 10);
        assert_eq!(m["build"]["peak_rss_mib"], 6.0);
        assert!(merge_bands(vec![band(1, 7)]).is_err());
    }

    #[test]
    fn sums_comaps_maps() {
        let tree = serde_json::json!({"id": "Countries", "g": [
            {"id": "US", "g": [{"id": "Ohio", "s": 10}, {"id": "US_Guam", "s": 2}]},
            {"id": "Canada", "g": [{"id": "Canada_Quebec", "g": [{"id": "Q1", "s": 5}]}]}
        ]});
        assert_eq!(comaps_bytes(&tree, "Countries").unwrap(), 17.0);
        assert_eq!(comaps_bytes(&tree, "US,-US_Guam,Canada").unwrap(), 15.0);
        assert_eq!(comaps_bytes(&tree, "Canada_Quebec").unwrap(), 5.0);
        assert!(comaps_bytes(&tree, "Mexico").is_err());
    }

    #[test]
    fn estimates_ch_from_regional_ratio() {
        let built = serde_json::json!({"routing": {"nodes": 10, "edges": 100,
            "ch_edges": 150, "ch_bytes_compact": 2000}});
        let graph = serde_json::json!({"routing": {"nodes": 10, "edges": 200}});
        let k = ch_ratio(&[built.clone(), graph.clone()]);
        assert_eq!(k, Some(1.5));
        assert_eq!(ch_bytes(&built, k), Some((2000.0, false)));
        assert_eq!(ch_bytes(&graph, k), Some(((300.0 + 10.0) * 12.0, true)));
        assert_eq!(ch_bytes(&graph, None), None);
    }
}
