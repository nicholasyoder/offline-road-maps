use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod addr;
mod admin;
mod build;
mod fetch;
mod measure;
mod report;
mod street;

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
    /// Phase 0 spike: tile a PBF through the whitelist and report sizes per layer, zoom and encoding.
    Measure(measure::MeasureArgs),
    /// Read a PBF's admin areas and places and report counts, chains and section sizes.
    Admin(admin::AdminArgs),
    /// Read a PBF's streets and report the street table's counts and section sizes.
    Streets(street::StreetsArgs),
    /// Phase 0 spike: merge address sources onto OSM streets and measure coverage and size.
    AddrSpike(addr::AddrArgs),
    /// Print Markdown size tables from `measure` or `addr-spike` JSON files.
    Report(report::ReportArgs),
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
        Command::Measure(args) => measure::run(args, &dir)?,
        Command::Admin(args) => admin::run(args)?,
        Command::Streets(args) => street::run(args)?,
        Command::AddrSpike(args) => addr::run(args, &dir)?,
        Command::Report(args) => report::run(args)?,
    }
    Ok(())
}
