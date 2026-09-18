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
    /// Create and publish a verified repository snapshot.
    Backup {
        /// Hosted repository name.
        #[arg(required_unless_present = "repo_path", conflicts_with = "repo_path")]
        name: Option<String>,
        /// Explicit bare repository path (used by the post-receive hook).
        #[arg(long)]
        repo_path: Option<PathBuf>,
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
        Some(Commands::Backup { name, repo_path }) => {
            let config = refuge::config::Config::load()?;
            let manifest = match (name, repo_path) {
                (Some(name), None) => refuge::backup::backup_named(&config, &name)?,
                (None, Some(path)) => refuge::backup::backup_path(&config, &path)?,
                _ => unreachable!("clap validates backup arguments"),
            };
            print_protected(&manifest);
        }
        Some(Commands::Hook {
            command: HookCommands::PostReceive,
        }) => {
            let result = refuge::config::Config::load().and_then(|config| {
                let path = std::env::current_dir()?;
                refuge::backup::backup_path(&config, &path)
            });
            match result {
                Ok(manifest) => print_protected(&manifest),
                Err(error) => eprintln!("refuge: backup failed: {error:#}"),
            }
        }
        None => {}
    }
    Ok(())
}

fn print_protected(manifest: &refuge::manifest::Manifest) {
    let size = manifest
        .artifact
        .as_ref()
        .map(|artifact| artifact.size)
        .unwrap_or(0);
    let ref_count = manifest
        .refs
        .len()
        .saturating_sub(usize::from(manifest.head().is_some()));
    println!(
        "protected {} {} refs {} bytes",
        manifest.snapshot_id, ref_count, size
    );
}
