use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "refuge", version, about = "Local-first Git repository backup")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize Refuge's local configuration.
    Init {
        /// Directory that will contain live bare repositories.
        #[arg(long)]
        repos: Option<PathBuf>,
        /// Directory that will receive immutable backup snapshots.
        #[arg(long)]
        target: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Commands::Init { repos, target }) = cli.command {
        let (_, path) = refuge::config::initialize(repos, target)?;
        println!("initialized refuge at {}", path.display());
    }
    Ok(())
}
