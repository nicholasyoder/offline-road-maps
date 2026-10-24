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
        print!("{}", addr_report(&runs));
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
