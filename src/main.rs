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
    /// Show whether hosted repositories match their newest snapshots.
    Status { name: Option<String> },
    /// Inspect published snapshots.
    Snapshots {
        #[command(subcommand)]
        command: SnapshotCommands,
    },
    /// Restore a repository from a verified snapshot.
    Restore {
        name_or_repo_id: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long = "as")]
        as_name: Option<String>,
        #[arg(long)]
        target: Option<PathBuf>,
        #[arg(long)]
        replace: bool,
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

#[derive(Debug, Subcommand)]
enum SnapshotCommands {
    /// List snapshot manifests and validate artifact presence and size.
    List {
        name_or_repo_id: Option<String>,
        #[arg(long)]
        target: Option<PathBuf>,
    },
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
        Some(Commands::Status { name }) => {
            let config = refuge::config::Config::load()?;
            for (repository, state) in refuge::discovery::statuses(&config, name.as_deref())? {
                let description = match state {
                    refuge::discovery::ProtectionState::Protected { snapshot_id } => {
                        format!("Protected ({snapshot_id})")
                    }
                    refuge::discovery::ProtectionState::Pending => {
                        "Pending (run `refuge backup`)".to_owned()
                    }
                    refuge::discovery::ProtectionState::Unprotected => "Unprotected".to_owned(),
                    refuge::discovery::ProtectionState::Corrupt { reason } => {
                        format!("Unprotected (corrupt: {reason})")
                    }
                };
                println!("{}: {description}", repository.name);
            }
        }
        Some(Commands::Snapshots {
            command:
                SnapshotCommands::List {
                    name_or_repo_id,
                    target,
                },
        }) => {
            let config = refuge::config::Config::load()?;
            let target = target.as_deref().unwrap_or(&config.target_root);
            for snapshot in refuge::discovery::list_snapshots(target, name_or_repo_id.as_deref())? {
                println!(
                    "{} {} generation {} {}",
                    snapshot.manifest.repo_name,
                    snapshot.manifest.snapshot_id,
                    snapshot.manifest.generation,
                    snapshot.health
                );
            }
        }
        Some(Commands::Restore {
            name_or_repo_id,
            snapshot,
            as_name,
            target,
            replace,
        }) => {
            let config = refuge::config::Config::load()?;
            let target = target.as_deref().unwrap_or(&config.target_root);
            let restored = refuge::restore::restore(
                &config,
                refuge::restore::RestoreOptions {
                    selector: &name_or_repo_id,
                    snapshot_id: snapshot.as_deref(),
                    as_name: as_name.as_deref(),
                    target,
                    replace,
                },
            )?;
            println!(
                "restored {} from {} at {}",
                restored.name,
                restored.snapshot_id,
                restored.path.display()
            );
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
