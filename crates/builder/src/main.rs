use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
}

fn work_dir(cli: &Cli) -> PathBuf {
    cli.work_dir
        .clone()
        .or_else(|| std::env::var_os("OMP_WORK").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Paths => {
            let dir = work_dir(&cli);
            let state = if dir.is_dir() { "exists" } else { "missing" };
            println!("work dir: {} ({state})", dir.display());
        }
    }
    Ok(())
}
