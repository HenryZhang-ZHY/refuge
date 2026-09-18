use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};

const ROOT_HELP: &str = "Typical workflow:
  1. refuge init --repos <LOCAL_DIR> --target <SYNC_DIR>
  2. refuge repo create <NAME>
  3. Run the printed `git remote add refuge ...` command in your working copy
  4. git push refuge main
  5. refuge status <NAME>

Refuge verifies snapshots written to the target directory. Cloud upload is not verified;
your sync client remains responsible for uploading that directory.";

#[derive(Debug, Parser)]
#[command(
    name = "refuge",
    version,
    about = "Local-first Git repository backup",
    long_about = "Host live Git repositories outside sync folders and publish verified, immutable snapshots to a filesystem target.",
    arg_required_else_help = true,
    after_help = ROOT_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize Refuge's local configuration.
    #[command(after_help = "Example:\n  refuge init --repos <LOCAL_DIR> --target <SYNC_DIR>")]
    Init {
        /// Directory that will contain live bare repositories.
        #[arg(long, value_name = "LOCAL_DIR")]
        repos: Option<PathBuf>,
        /// Directory that will receive immutable backup snapshots.
        #[arg(long, value_name = "SYNC_DIR")]
        target: PathBuf,
    },
    /// Create or import hosted repositories.
    #[command(
        after_help = "Examples:\n  refuge repo create notes\n  refuge repo import notes <EXISTING_REPO>"
    )]
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
    /// Create and publish a verified repository snapshot.
    #[command(
        after_help = "Examples:\n  refuge backup notes\n  refuge backup --repo-path <BARE_REPO>"
    )]
    Backup {
        /// Hosted repository name.
        #[arg(
            value_name = "NAME",
            required_unless_present = "repo_path",
            conflicts_with = "repo_path"
        )]
        name: Option<String>,
        /// Explicit bare repository path (used by the post-receive hook).
        #[arg(long, value_name = "BARE_REPO")]
        repo_path: Option<PathBuf>,
    },
    /// Show whether hosted repositories match their newest snapshots.
    #[command(after_help = "Examples:\n  refuge status\n  refuge status notes")]
    Status {
        /// Repository name. Omit it to show every hosted repository.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Inspect published snapshots.
    Snapshots {
        #[command(subcommand)]
        command: SnapshotCommands,
    },
    /// Restore a repository from a verified snapshot.
    #[command(
        after_help = "Examples:\n  refuge restore notes\n  refuge restore <REPO_ID> --snapshot <SNAPSHOT_ID> --as recovered-notes"
    )]
    Restore {
        /// Repository name or stable UUID to discover in the target.
        #[arg(value_name = "NAME_OR_REPO_ID")]
        name_or_repo_id: String,
        /// Restore this snapshot ID instead of the newest valid snapshot.
        #[arg(long, value_name = "SNAPSHOT_ID")]
        snapshot: Option<String>,
        /// Register the restored repository under a different local name.
        #[arg(long = "as", value_name = "NAME")]
        as_name: Option<String>,
        /// Read snapshots from this target instead of the configured target.
        #[arg(long, value_name = "SYNC_DIR")]
        target: Option<PathBuf>,
        /// Replace an existing hosted repository after the snapshot is verified.
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
    #[command(after_help = "Example:\n  refuge repo create notes")]
    Create {
        /// Repository name (letters, digits, dots, dashes, and underscores).
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Import an existing repository with mirror semantics.
    #[command(after_help = "Example:\n  refuge repo import notes <EXISTING_REPO>")]
    Import {
        /// Name to give the hosted repository.
        #[arg(value_name = "NAME")]
        name: String,
        /// Existing Git working tree or bare repository to mirror.
        #[arg(value_name = "EXISTING_REPO")]
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum HookCommands {
    PostReceive,
}

#[derive(Debug, Subcommand)]
enum SnapshotCommands {
    /// List snapshot manifests and validate artifact presence and size.
    #[command(
        after_help = "Examples:\n  refuge snapshots list\n  refuge snapshots list notes\n  refuge snapshots list <REPO_ID> --target <SYNC_DIR>"
    )]
    List {
        /// Repository name or stable UUID. Omit it to list all snapshots.
        #[arg(value_name = "NAME_OR_REPO_ID")]
        name_or_repo_id: Option<String>,
        /// Read from this target instead of the configured target.
        #[arg(long, value_name = "SYNC_DIR")]
        target: Option<PathBuf>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("refuge: {error:#}");
        process::exit(2);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { repos, target } => {
            let (config, path) = refuge::config::initialize(repos, Some(target))?;
            println!("initialized refuge at {}", path.display());
            println!("Repositories: {}", config.repos_dir.display());
            println!("Backup target: {}", config.target_root.display());
            println!(
                "Cloud upload is not verified by Refuge; the sync client is responsible for uploading this target."
            );
            println!("Next: refuge repo create <name>");
        }
        Commands::Repo { command } => {
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
        Commands::Backup { name, repo_path } => {
            let config = refuge::config::Config::load()?;
            let manifest = match (name, repo_path) {
                (Some(name), None) => refuge::backup::backup_named(&config, &name)?,
                (None, Some(path)) => refuge::backup::backup_path(&config, &path)?,
                _ => unreachable!("clap validates backup arguments"),
            };
            print_protected(&manifest);
        }
        Commands::Status { name } => {
            let config = refuge::config::Config::load()?;
            for (repository, state) in refuge::discovery::statuses(&config, name.as_deref())? {
                let description = match state {
                    refuge::discovery::ProtectionState::Protected { snapshot_id } => {
                        format!(
                            "Protected locally ({snapshot_id})\n  Cloud upload is not verified by Refuge"
                        )
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
        Commands::Snapshots {
            command:
                SnapshotCommands::List {
                    name_or_repo_id,
                    target,
                },
        } => {
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
        Commands::Restore {
            name_or_repo_id,
            snapshot,
            as_name,
            target,
            replace,
        } => {
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
        Commands::Hook {
            command: HookCommands::PostReceive,
        } => {
            let result = refuge::config::Config::load().and_then(|config| {
                let path = std::env::current_dir()?;
                refuge::backup::backup_path(&config, &path)
            });
            match result {
                Ok(manifest) => print_protected(&manifest),
                Err(error) => eprintln!("refuge: backup failed: {error:#}"),
            }
        }
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
