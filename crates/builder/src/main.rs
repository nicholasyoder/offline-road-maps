use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod fetch;
mod measure;
mod report;

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
    /// Phase 0 spike: tile a PBF through the whitelist and report sizes per layer, zoom and encoding.
    Measure(measure::MeasureArgs),
    /// Print Markdown size tables from `measure` JSON files.
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
        Command::Measure(args) => measure::run(args, &dir)?,
        Command::Report(args) => report::run(args)?,
    }
    Ok(())
}
