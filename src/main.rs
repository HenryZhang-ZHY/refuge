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
    /// Create or import hosted repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        command: HookCommands,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCommands {
    /// Create a new empty bare repository.
    Create { name: String },
    /// Import an existing repository with mirror semantics.
    Import { name: String, path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum HookCommands {
    PostReceive,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Init { repos, target }) => {
            let (_, path) = refuge::config::initialize(repos, target)?;
            println!("initialized refuge at {}", path.display());
        }
        Some(Commands::Repo { command }) => {
            let config = refuge::config::Config::load()?;
            let repository = match command {
                RepoCommands::Create { name } => refuge::repo::create(&config, &name)?,
                RepoCommands::Import { name, path } => refuge::repo::import(&config, &name, &path)?,
            };
            println!(
                "created repository {}\ngit remote add refuge \"{}\"",
                repository.id,
                repository.path.display()
            );
        }
        Some(Commands::Hook {
            command: HookCommands::PostReceive,
        }) => {}
        None => {}
    }
    Ok(())
}
