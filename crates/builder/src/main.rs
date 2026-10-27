use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod addr;
mod admin;
mod build;
mod ch;
mod fetch;
mod find;
mod geom;
mod graph;
mod measure;
mod pbf;
mod poly;
mod report;
mod search;
mod street;
mod tiles;

/// Builds offline map packs from OSM and address sources.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Work directory for inputs, temp files and outputs [default: $OMP_WORK, else ./data]
    #[arg(long, global = true)]
    work_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the resolved work directory and whether it exists.
    Paths,
    /// Download a region's OSM extract and address sources into <work dir>/inputs.
    Fetch(fetch::FetchArgs),
    /// Cut a PBF into tiles and write a pack.
    Build(build::BuildArgs),
    /// Print a pack's header, sections, schema and tile counts, or one decoded tile.
    Inspect(build::inspect::InspectArgs),
    /// Check every section's checksum.
    Verify(build::inspect::VerifyArgs),
    /// Search a pack as the app does, or run a query set (`--check`).
    Search(find::SearchArgs),
    /// What is at a point: the nearest address and street, entries close by, admin areas.
    Here(find::HereArgs),
    /// Phase 0 spike: tile a PBF through the whitelist and report sizes per layer, zoom and encoding.
    Measure(measure::MeasureArgs),
    /// Read a PBF's admin areas and places and report counts, chains and section sizes.
    Admin(admin::AdminArgs),
    /// Read a PBF's streets and report the street table's counts and section sizes.
    Streets(street::StreetsArgs),
    /// Build a PBF's car routing graph and report its counts.
    Graph(graph::GraphArgs),
    /// Contract a car graph into a contraction hierarchy and check it against Dijkstra.
    Ch(ch::check::ChArgs),
    /// Print Markdown size tables from `measure` JSON files.
    Report(report::ReportArgs),
}

/// Bytes in MiB, for reports.
fn mib(b: u64) -> f64 {
    b as f64 / (1u64 << 20) as f64
}

/// Peak resident set size so far, from /proc (Linux only; 0 elsewhere).
fn peak_rss_mib() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn work_dir(cli: &Cli) -> PathBuf {
    cli.work_dir
        .clone()
        .or_else(|| std::env::var_os("OMP_WORK").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dir = work_dir(&cli);
    match cli.command {
        Command::Paths => {
            let state = if dir.is_dir() { "exists" } else { "missing" };
            println!("work dir: {} ({state})", dir.display());
        }
        Command::Fetch(args) => fetch::run(args, &dir)?,
        Command::Build(args) => build::run(args, &dir)?,
        Command::Inspect(args) => build::inspect::inspect(args)?,
        Command::Verify(args) => build::inspect::verify(args)?,
        Command::Search(args) => find::search(args)?,
        Command::Here(args) => find::here(args)?,
        Command::Measure(args) => measure::run(args, &dir)?,
        Command::Admin(args) => admin::run(args)?,
        Command::Streets(args) => street::run(args)?,
        Command::Graph(args) => graph::run(args)?,
        Command::Ch(args) => ch::check::run(args)?,
        Command::Report(args) => report::run(args)?,
    }
    Ok(())
}
