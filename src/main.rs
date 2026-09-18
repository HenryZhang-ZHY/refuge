use std::ffi::OsString;
use std::path::PathBuf;
use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};

const ROOT_HELP: &str = "Typical workflow:
  1. refuge init --target <SYNC_DIR>
  2. refuge repo create <NAME>
  3. Run the printed `git remote add refuge ...` command in your working copy
  4. git push refuge main
  5. refuge status <NAME>

Refuge verifies snapshots written to the target directory. Cloud upload is not verified;
your sync client remains responsible for uploading that directory.";

#[derive(Debug, Parser)]
#[command(
    name = "refuge",
    bin_name = "refuge",
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
    #[command(
        after_help = "Example:\n  refuge init --target <SYNC_DIR>\n  refuge init --repos <LOCAL_DIR> --target <SYNC_DIR>"
    )]
    Init {
        /// Directory that will contain live bare repositories. Defaults to
        /// the user data directory (e.g. ~/.local/share/refuge/repos, or
        /// %LOCALAPPDATA%\refuge\repos on Windows).
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
        /// Clone the new repository, optionally into DIRECTORY.
        #[arg(long, value_name = "DIRECTORY", num_args = 0..=1)]
        clone: Option<Option<PathBuf>>,
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
        /// Add the hosted repository as a remote in the imported working tree.
        #[arg(long)]
        connect: bool,
        /// Remote name used with --connect.
        #[arg(long, default_value = "refuge", value_name = "NAME")]
        remote: String,
    },
    /// List hosted repositories.
    #[command(alias = "ls")]
    List,
    /// Clone a hosted repository into a working directory.
    #[command(
        after_help = "Examples:\n  refuge repo clone notes\n  refuge repo clone notes <DIRECTORY>\n  refuge repo clone notes -- --depth=1"
    )]
    Clone {
        /// Hosted repository name or stable UUID.
        #[arg(value_name = "NAME_OR_REPO_ID")]
        selector: String,
        /// Working directory to create. Defaults to the repository name.
        #[arg(value_name = "DIRECTORY")]
        directory: Option<PathBuf>,
        /// Name for the remote created by git clone.
        #[arg(long, default_value = "origin", value_name = "NAME")]
        remote_name: String,
        /// Additional arguments passed to git clone after `--`.
        #[arg(last = true, allow_hyphen_values = true, value_name = "GIT_ARGS")]
        git_args: Vec<OsString>,
    },
    /// Connect the current working copy to a hosted repository.
    #[command(after_help = "Example:\n  refuge repo connect notes")]
    Connect {
        /// Hosted repository name or stable UUID.
        #[arg(value_name = "NAME_OR_REPO_ID")]
        selector: String,
        /// Name of the Git remote to add.
        #[arg(long, default_value = "refuge", value_name = "NAME")]
        remote: String,
        /// Replace an existing remote that has a different URL.
        #[arg(long)]
        replace: bool,
    },
    /// Show a hosted repository and its protection state.
    View {
        /// Repository name, stable UUID, or `.` for the current working copy.
        #[arg(value_name = "NAME_OR_REPO_ID", default_value = ".")]
        selector: String,
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
            match command {
                RepoCommands::Create { name, clone } => {
                    let repository = refuge::repo::create(&config, &name)?;
                    println!(
                        "created repository {}\ngit remote add refuge \"{}\"",
                        repository.id,
                        repository.path.display()
                    );
                    if let Some(directory) = clone {
                        let directory = directory.unwrap_or_else(|| PathBuf::from(&name));
                        refuge::git::clone_working(&repository.path, &directory, "origin", &[])?;
                        println!("cloned {name} to {}", directory.display());
                    }
                }
                RepoCommands::Import {
                    name,
                    path,
                    connect,
                    remote,
                } => {
                    let repository = refuge::repo::import(&config, &name, &path)?;
                    println!(
                        "created repository {}\ngit remote add refuge \"{}\"",
                        repository.id,
                        repository.path.display()
                    );
                    if connect {
                        let (hosted, _) =
                            refuge::repo::connect_at(&config, &name, &remote, false, &path)?;
                        println!(
                            "connected remote `{remote}` to {} ({})",
                            hosted.name, hosted.id
                        );
                        let manifest = refuge::backup::backup_path(&config, &repository.path)?;
                        print_protected(&manifest);
                    }
                }
                RepoCommands::List => {
                    for repository in refuge::repo::list(&config)? {
                        println!(
                            "{} {} {}",
                            repository.name,
                            repository.id,
                            repository.path.display()
                        );
                    }
                }
                RepoCommands::Clone {
                    selector,
                    directory,
                    remote_name,
                    git_args,
                } => {
                    let repository = refuge::repo::resolve(&config, &selector)?;
                    let directory = directory.unwrap_or_else(|| PathBuf::from(&repository.name));
                    refuge::git::clone_working(
                        &repository.path,
                        &directory,
                        &remote_name,
                        &git_args,
                    )?;
                    println!("cloned {} to {}", repository.name, directory.display());
                }
                RepoCommands::Connect {
                    selector,
                    remote,
                    replace,
                } => {
                    let (repository, _) =
                        refuge::repo::connect(&config, &selector, &remote, replace)?;
                    println!(
                        "connected remote `{remote}` to {} ({})\nNext: git push {remote} main",
                        repository.name, repository.id
                    );
                }
                RepoCommands::View { selector } => {
                    let (repository, remote) = if selector == "." {
                        let (repository, remote) = refuge::repo::current(&config)?;
                        (repository, Some(remote))
                    } else {
                        (refuge::repo::resolve(&config, &selector)?, None)
                    };
                    let state = refuge::discovery::repository_status(&config, &repository)?;
                    let head = refuge::git::ref_state(&repository.path)?
                        .head
                        .unwrap_or_else(|| "(detached)".to_owned());
                    println!("Name: {}", repository.name);
                    println!("ID: {}", repository.id);
                    println!("Repository: {}", repository.path.display());
                    if let Some(remote) = remote {
                        println!("Remote: {remote}");
                    }
                    println!("Default branch: {head}");
                    println!("Status: {}", protection_description(state));
                }
            }
        }
        Commands::Backup { name, repo_path } => {
            let config = refuge::config::Config::load()?;
            let manifest = match (name, repo_path) {
                (Some(selector), None) => {
                    let repository = refuge::repo::resolve(&config, &selector)?;
                    refuge::backup::backup_path(&config, &repository.path)?
                }
                (None, Some(path)) => refuge::backup::backup_path(&config, &path)?,
                _ => unreachable!("clap validates backup arguments"),
            };
            print_protected(&manifest);
        }
        Commands::Status { name } => {
            let config = refuge::config::Config::load()?;
            for (repository, state) in refuge::discovery::statuses(&config, name.as_deref())? {
                let description = protection_description(state);
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
    match &manifest.lfs_artifact {
        Some(lfs) => println!(
            "protected {} {} refs {} bytes, {} LFS bytes",
            manifest.snapshot_id, ref_count, size, lfs.size
        ),
        None => println!(
            "protected {} {} refs {} bytes",
            manifest.snapshot_id, ref_count, size
        ),
    }
}

fn protection_description(state: refuge::discovery::ProtectionState) -> String {
    match state {
        refuge::discovery::ProtectionState::Protected { snapshot_id } => {
            format!("Protected locally ({snapshot_id})\n  Cloud upload is not verified by Refuge")
        }
        refuge::discovery::ProtectionState::Pending => "Pending (run `refuge backup`)".to_owned(),
        refuge::discovery::ProtectionState::Unprotected => "Unprotected".to_owned(),
        refuge::discovery::ProtectionState::Corrupt { reason } => {
            format!("Unprotected (corrupt: {reason})")
        }
    }
}
