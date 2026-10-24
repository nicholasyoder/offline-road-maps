//! `omp-builder report`: turns `measure` JSON files into Markdown size tables.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

#[derive(Args)]
pub struct ReportArgs {
    /// JSON files written by `measure`, one per region.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
}

const VARIANTS: [&str; 6] = ["v0", "v1", "v2", "v3", "v4", "v5"];
/// The variant used for the per-layer and per-zoom tables.
const MAIN: &str = "v4";

fn mib(b: f64) -> String {
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
        mib(num(&r["input_bytes"]))
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
        mib(num(&r["build"]["node_index_disk_bytes"]))
    });
    row(&mut out, "Tile store temp (MiB)", &|r| {
        mib(num(&r["build"]["tile_store_bytes"]))
    });

    let _ = writeln!(out, "\n## Tiles z0–z14 by variant (MiB)\n");
    header(&mut out, "Variant");
    for v in VARIANTS {
        let _ = write!(out, "| {} |", v.to_uppercase());
        for c in &cells {
            let total: f64 = c.iter().filter(|(k, _)| k.0 == v).map(|(_, b)| b).sum();
            let _ = write!(out, " {} |", mib(total));
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
        let _ = write!(out, " {} |", mib(d));
    }
    let _ = write!(out, "\n| MVT + gzip (V2 geometry) |");
    for r in &runs {
        let gz: f64 = r["mvt"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| num(&m["gzip_bytes"]))
            .sum();
        let _ = write!(out, " {} |", mib(gz));
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
            let _ = write!(out, " {} |", mib(total));
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
            let _ = write!(out, " {} |", mib(total));
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
    row(&mut out, "CH prepare (s)", &|r| {
        format!("{:.0}", num(&r["routing"]["ch_prepare_s"]))
    });
    row(&mut out, "CH, compact layout (MiB)", &|r| {
        mib(num(&r["routing"]["ch_bytes_compact"]))
    });
    row(&mut out, "Search fst (MiB)", &|r| {
        mib(num(&r["search"]["fst_bytes"]))
    });
    row(&mut out, "Search postings (MiB)", &|r| {
        mib(num(&r["search"]["postings_bytes"]))
    });
    row(&mut out, "Search entries (MiB)", &|r| {
        mib(num(&r["search"]["entries_bytes"]))
    });
    print!("{out}");
    Ok(())
}
