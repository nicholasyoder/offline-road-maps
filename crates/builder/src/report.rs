//! `omp-builder report`: turns `measure` or `addr-spike` JSON files into Markdown tables.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

#[derive(Args)]
pub struct ReportArgs {
    /// JSON files written by `measure` (or all by `addr-spike`), one per region.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Sum `addr-spike` runs (e.g. one per state and province) into one column with this name.
    #[arg(long)]
    sum: Option<String>,

    /// With `--sum`, also write the summed run here, to report it next to other runs.
    #[arg(long, requires = "sum")]
    sum_json: Option<PathBuf>,
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
    if runs.iter().all(|r| r["kind"] == "addr") {
        if let Some(name) = &args.sum {
            let total = sum_addr_runs(&runs, name);
            if let Some(p) = &args.sum_json {
                std::fs::write(p, serde_json::to_string_pretty(&total)?)
                    .with_context(|| format!("write {}", p.display()))?;
            }
            runs = vec![total];
        }
        print!("{}", addr_report(&runs));
        return Ok(());
    }
    anyhow::ensure!(args.sum.is_none(), "--sum only applies to addr-spike runs");
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
    let has_ch = |r: &Value| r["routing"]["ch_edges"].is_number();
    // For runs that only counted the graph (`--routing graph`), the CH is estimated from the
    // mean CH-edges-per-edge ratio of the runs that built one.
    let ratios: Vec<f64> = runs
        .iter()
        .filter(|r| has_ch(r))
        .map(|r| num(&r["routing"]["ch_edges"]) / num(&r["routing"]["edges"]).max(1.0))
        .collect();
    let ratio = (!ratios.is_empty()).then(|| ratios.iter().sum::<f64>() / ratios.len() as f64);
    row(&mut out, "CH prepare (s)", &|r| {
        if has_ch(r) {
            format!("{:.0}", num(&r["routing"]["ch_prepare_s"]))
        } else {
            "–".into()
        }
    });
    row(&mut out, "CH, compact layout (MiB)", &|r| {
        let g = &r["routing"];
        match ratio {
            _ if has_ch(r) => mib(num(&g["ch_bytes_compact"])),
            Some(k) if g["edges"].is_number() => format!(
                "~{} (est., {k:.2} CH edges per edge)",
                mib(num(&g["edges"]) * k * 12.0 + num(&g["nodes"]) * 12.0)
            ),
            _ => "–".into(),
        }
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

fn pct(a: f64, b: f64) -> String {
    if b > 0.0 {
        format!("{:.1}%", a / b * 100.0)
    } else {
        "–".into()
    }
}

/// Fields that identify a row in a list; rows with the same values are summed together.
const ROW_KEYS: [&str; 7] = [
    "source",
    "license",
    "variant",
    "dedup_m",
    "block_kib",
    "path",
    "name",
];
/// Settings, kept from the first run rather than summed.
const SETTINGS: [&str; 3] = ["attach_m", "dedup_m", "zstd_level"];

/// Adds `b` into `a`: numbers add (peak RSS takes the maximum), lists of rows are matched by
/// [`ROW_KEYS`], number lists add element-wise, and strings keep `a`'s value.
fn add_value(a: &mut Value, b: &Value, key: &str) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if !SETTINGS.contains(&key) => {
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

/// One run that adds up `runs`, with ratios recomputed from the sums.
fn sum_addr_runs(runs: &[Value], name: &str) -> Value {
    let mut total = runs[0].clone();
    for r in &runs[1..] {
        add_value(&mut total, r, "");
    }
    total["region"] = Value::from(name);
    let regions: Vec<Value> = runs.iter().map(|r| r["region"].clone()).collect();
    total["summed_regions"] = Value::from(regions);
    let addresses = num(&total["encoding"]["addresses"]).max(1.0);
    for v in total["encoding"]["variants"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        v["bytes_per_address"] = Value::from(num(&v["total"]) / addresses);
    }
    // Runs differ in their sources (no NAR in the US), so cumulative counts are re-added.
    if let Some(cov) = total["coverage"].as_array_mut() {
        let pos = |r: &Value| ADDR_SOURCES.iter().position(|(id, _)| r["source"] == *id);
        cov.sort_by_key(pos);
        let mut cumulative = 0;
        for r in cov {
            cumulative += r["new_addresses"].as_u64().unwrap_or(0);
            r["cumulative"] = Value::from(cumulative);
        }
    }
    for r in total["ranges"].as_array_mut().into_iter().flatten() {
        let n = num(&r["ranges"]).max(1.0);
        r["share_with_osm_point"] = Value::from(num(&r["with_osm_point"]) / n);
        r["share_with_merged_point"] = Value::from(num(&r["with_merged_point"]) / n);
    }
    total
}

fn find<'a>(list: &'a Value, key: &str, value: &Value) -> Option<&'a Value> {
    list.as_array()?.iter().find(|x| &x[key] == value)
}

const ADDR_SOURCES: [(&str, &str); 5] = [
    ("osm", "OSM"),
    ("open_addresses", "OpenAddresses"),
    ("nar", "NAR (building point)"),
    ("oda", "ODA"),
    ("nar_blockface", "NAR (blockface only)"),
];

fn addr_report(runs: &[Value]) -> String {
    let mut out = String::new();
    let header = |out: &mut String, first: &str| {
        let _ = write!(out, "| {first} |");
        for r in runs {
            let _ = write!(out, " {} |", r["region"].as_str().unwrap_or("?"));
        }
        let _ = write!(out, "\n|---|");
        for _ in runs {
            out.push_str("---:|");
        }
        out.push('\n');
    };
    let row = |out: &mut String, label: &str, f: &dyn Fn(&Value) -> String| {
        let _ = write!(out, "| {label} |");
        for r in runs {
            let _ = write!(out, " {} |", f(r));
        }
        out.push('\n');
    };
    let dash = || "–".to_owned();

    let _ = writeln!(out, "## Build\n");
    header(&mut out, "");
    row(&mut out, "Wall time (s)", &|r| {
        format!("{:.0}", num(&r["wall_s"]))
    });
    row(&mut out, "Peak RSS (MiB)", &|r| {
        format!("{:.0}", num(&r["peak_rss_mib"]))
    });
    row(&mut out, "Attach / dedup distance (m)", &|r| {
        format!("{:.0} / {:.0}", num(&r["attach_m"]), num(&r["dedup_m"]))
    });

    let _ = writeln!(out, "\n## Records read\n");
    header(&mut out, "Source");
    for (id, label) in ADDR_SOURCES {
        let src = Value::from(id);
        row(&mut out, label, &|r| {
            find(&r["sources"], "source", &src).map_or_else(dash, |s| count(&s["records"]))
        });
    }
    row(&mut out, "TIGER ranges", &|r| {
        find(&r["ranges"], "source", &Value::from("tiger"))
            .map_or_else(dash, |s| count(&s["ranges"]))
    });
    row(&mut out, "OSM interpolation ranges", &|r| {
        find(&r["ranges"], "source", &Value::from("osm_interpolation"))
            .map_or_else(dash, |s| count(&s["ranges"]))
    });

    let _ = writeln!(
        out,
        "\n## Normalization exceptions (share of the source's records)\n"
    );
    header(&mut out, "Source: empty / unparseable / 0 / no street");
    for (id, label) in ADDR_SOURCES {
        let src = Value::from(id);
        row(&mut out, label, &|r| {
            find(&r["sources"], "source", &src).map_or_else(dash, |s| {
                let n = num(&s["records"]);
                format!(
                    "{} / {} / {} / {}",
                    pct(num(&s["no_number"]), n),
                    pct(num(&s["number_exceptions"]), n),
                    pct(num(&s["zero_number"]), n),
                    pct(num(&s["no_street"]), n)
                )
            })
        });
    }

    let _ = writeln!(
        out,
        "\n## Street attach: share attached at 100 / 200 / 500 m (exact key + loose key)\n"
    );
    header(&mut out, "Source");
    for (id, label) in ADDR_SOURCES {
        let src = Value::from(id);
        row(&mut out, label, &|r| {
            find(&r["attach"], "source", &src).map_or_else(dash, |a| {
                let n = num(&a["records"]);
                (0..3)
                    .map(|i| pct(num(&a["strict"][i]) + num(&a["loose"][i]), n))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
        });
        row(&mut out, &format!("  {label}, exact key only"), &|r| {
            find(&r["attach"], "source", &src).map_or_else(dash, |a| {
                let n = num(&a["records"]);
                (0..3)
                    .map(|i| pct(num(&a["strict"][i]), n))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
        });
    }

    let _ = writeln!(out, "\n## Unique addresses after the merge (cumulative)\n");
    header(&mut out, "Sources");
    for (i, (id, label)) in ADDR_SOURCES.iter().enumerate() {
        let src = Value::from(*id);
        let name = if i == 0 {
            format!("{label} only")
        } else {
            format!("+ {label}")
        };
        row(&mut out, &name, &|r| {
            find(&r["coverage"], "source", &src).map_or_else(dash, |c| {
                format!(
                    "{} (+{})",
                    count(&c["cumulative"]),
                    count(&c["new_addresses"])
                )
            })
        });
    }
    row(&mut out, "Units (distinct, all sources)", &|r| {
        count(&r["merged"]["units"])
    });
    row(&mut out, "Attached to a street", &|r| {
        pct(
            num(&r["merged"]["attached"]),
            num(&r["merged"]["addresses"]),
        )
    });
    row(&mut out, "Addresses per km of addressed street", &|r| {
        format!(
            "{:.0}",
            num(&r["merged"]["addresses"]) / num(&r["merged"]["street_km_with_addresses"]).max(1.0)
        )
    });

    let _ = writeln!(out, "\n## Dedup by distance\n");
    header(&mut out, "Distance: unique addresses / conflicts");
    for d in [25.0, 50.0, 100.0, 250.0, 1000.0] {
        let dv = Value::from(d);
        row(&mut out, &format!("{d:.0} m"), &|r| {
            find(&r["dedup"], "dedup_m", &dv).map_or_else(dash, |x| {
                let c = num(&x["conflicts_attached"]) + num(&x["conflicts_unattached"]);
                format!(
                    "{} / {}",
                    count(&x["addresses"]),
                    count(&Value::from(c as u64))
                )
            })
        });
    }

    let _ = writeln!(
        out,
        "\n## Ranges: share with at least one point within 100 m\n"
    );
    header(
        &mut out,
        "Ranges: OSM points / merged points / fallback ranges",
    );
    for (id, label) in [
        ("tiger", "TIGER"),
        ("osm_interpolation", "OSM interpolation"),
    ] {
        let src = Value::from(id);
        row(&mut out, label, &|r| {
            find(&r["ranges"], "source", &src).map_or_else(dash, |x| {
                format!(
                    "{} / {} / {}",
                    pct(num(&x["with_osm_point"]), num(&x["ranges"])),
                    pct(num(&x["with_merged_point"]), num(&x["ranges"])),
                    count(&x["fallback_ranges"])
                )
            })
        });
    }

    let _ = writeln!(out, "\n## Address store size (MiB; bytes per address)\n");
    header(&mut out, "Variant");
    for v in ["a1", "a2", "a3", "a4"] {
        let vv = Value::from(v);
        row(&mut out, &v.to_uppercase(), &|r| {
            find(&r["encoding"]["variants"], "variant", &vv).map_or_else(dash, |x| {
                format!(
                    "{} ({:.2})",
                    mib(num(&x["total"])),
                    num(&x["bytes_per_address"])
                )
            })
        });
    }
    let a3 = Value::from("a3");
    for (field, label) in [
        ("streets", "  A3 street runs"),
        ("units", "  A3 units table"),
        ("suffixes", "  A3 suffix table"),
        ("unattached", "  A3 unattached points"),
    ] {
        row(&mut out, label, &|r| {
            find(&r["encoding"]["variants"], "variant", &a3)
                .map_or_else(dash, |x| mib(num(&x[field])))
        });
    }
    row(&mut out, "TIGER fallback ranges (zstd)", &|r| {
        find(&r["encoding"]["ranges"], "source", &Value::from("tiger")).map_or_else(dash, |x| {
            format!(
                "{} ({:.1} B/range)",
                mib(num(&x["zstd"])),
                num(&x["zstd"]) / num(&x["ranges"]).max(1.0)
            )
        })
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_addr_runs() {
        let us: Value = serde_json::from_str(
            r#"{"region": "oh", "peak_rss_mib": 10.0, "dedup_m": 250.0,
                "coverage": [{"source": "osm", "new_addresses": 5, "cumulative": 5},
                             {"source": "open_addresses", "new_addresses": 10, "cumulative": 15}],
                "attach": [{"source": "osm", "records": 5, "strict": [1, 2, 3]}],
                "encoding": {"addresses": 15, "variants": [{"variant": "a3", "total": 30, "bytes_per_address": 2.0}]}}"#,
        )
        .unwrap();
        let ca: Value = serde_json::from_str(
            r#"{"region": "on", "peak_rss_mib": 20.0, "dedup_m": 250.0,
                "coverage": [{"source": "osm", "new_addresses": 1, "cumulative": 1},
                             {"source": "nar", "new_addresses": 4, "cumulative": 5}],
                "attach": [{"source": "osm", "records": 1, "strict": [1, 1, 1]}],
                "encoding": {"addresses": 5, "variants": [{"variant": "a3", "total": 20, "bytes_per_address": 4.0}]}}"#,
        )
        .unwrap();
        let t = sum_addr_runs(&[us, ca], "both");
        assert_eq!(t["region"], "both");
        assert_eq!(t["peak_rss_mib"], 20.0);
        assert_eq!(t["dedup_m"], 250.0);
        let cumulative: Vec<u64> = t["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["cumulative"].as_u64().unwrap())
            .collect();
        assert_eq!(cumulative, [6, 16, 20]);
        assert_eq!(t["attach"][0]["strict"], serde_json::json!([2, 3, 4]));
        assert_eq!(t["encoding"]["variants"][0]["bytes_per_address"], 2.5);
    }
}
