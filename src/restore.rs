use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::backup;
use crate::config::Config;
use crate::discovery::{self, Snapshot, SnapshotHealth};
use crate::{git, repo};

pub struct RestoreOptions<'a> {
    pub selector: &'a str,
    pub snapshot_id: Option<&'a str>,
    pub as_name: Option<&'a str>,
    pub target: &'a Path,
    pub replace: bool,
}

pub struct RestoredRepository {
    pub name: String,
    pub path: PathBuf,
    pub snapshot_id: String,
}

pub fn restore(config: &Config, options: RestoreOptions<'_>) -> Result<RestoredRepository> {
    let snapshots = discovery::list_snapshots(options.target, Some(options.selector))?;
    let snapshot = select_snapshot(snapshots, options.snapshot_id)?;
    let name = options
        .as_name
        .unwrap_or(&snapshot.manifest.repo_name)
        .to_owned();
    let destination = repo::destination(config, &name)?;
    if destination.exists() && !options.replace {
        bail!(
            "repository already exists at {}; pass --replace to replace it",
            destination.display()
        );
    }

    let artifact_path = discovery::artifact_path(options.target, &snapshot.manifest)?;
    if let (Some(artifact), Some(path)) = (&snapshot.manifest.artifact, &artifact_path) {
        let (checksum, size) = backup::checksum(path)?;
        if size != artifact.size || checksum != artifact.checksum {
            bail!("snapshot artifact checksum or size differs from its manifest");
        }

        // `git bundle verify` needs repository context even for a full bundle.
        // Verify in a disposable bare repository before touching a destination.
        let verification = tempfile::tempdir_in(&config.repos_dir)?;
        git::init_bare(verification.path())?;
        git::bundle_verify(verification.path(), path)?;
    }

    let lfs_archive_path = discovery::lfs_artifact_path(options.target, &snapshot.manifest)?;
    if let (Some(artifact), Some(path)) = (&snapshot.manifest.lfs_artifact, &lfs_archive_path) {
        let (checksum, size) = backup::checksum(path)?;
        if size != artifact.size || checksum != artifact.checksum {
            bail!("snapshot LFS artifact checksum or size differs from its manifest");
        }
    }

    // Build next to the destination and publish with a same-volume rename. A
    // failed clone never leaves a half-repository at the user-visible path.
    let staging = tempfile::tempdir_in(&config.repos_dir)?;
    let staged_repo = staging.path().join("repository.git");
    if let Some(path) = &artifact_path {
        git::clone_mirror(path, &staged_repo)?;
    } else {
        git::init_bare(&staged_repo)?;
    }
    if let Some(head) = snapshot.manifest.head() {
        git::set_symbolic_head(&staged_repo, head)?;
    }
    repo::configure(&staged_repo, snapshot.manifest.repo_id)?;
    if let Some(path) = &lfs_archive_path {
        extract_lfs_archive(path, &staged_repo)?;
    }

    publish_repository(&staged_repo, &destination, options.replace)?;
    Ok(RestoredRepository {
        name,
        path: destination,
        snapshot_id: snapshot.manifest.snapshot_id,
    })
}

/// Extracts a previously verified `lfs.tar` archive into
/// `<repo>/lfs/objects`, then re-verifies each extracted object's content
/// hash against its filename (its LFS object id) so a corrupted archive
/// never produces a silently broken restored repository.
fn extract_lfs_archive(archive: &Path, repo: &Path) -> Result<()> {
    let destination = repo.join("lfs").join("objects");
    fs::create_dir_all(&destination)
        .with_context(|| format!("could not create {}", destination.display()))?;
    let file =
        fs::File::open(archive).with_context(|| format!("could not open {}", archive.display()))?;
    tar::Archive::new(file)
        .unpack(&destination)
        .with_context(|| format!("could not extract {}", archive.display()))?;
    backup::verify_lfs_objects(&destination)
}

fn select_snapshot(snapshots: Vec<Snapshot>, selected: Option<&str>) -> Result<Snapshot> {
    let mut matching: Vec<_> = snapshots
        .into_iter()
        .filter(|snapshot| selected.is_none_or(|id| snapshot.manifest.snapshot_id == id))
        .collect();
    if matching.is_empty() {
        bail!("no matching snapshot found");
    }
    let first_repo = matching[0].manifest.repo_id;
    if matching
        .iter()
        .any(|snapshot| snapshot.manifest.repo_id != first_repo)
    {
        bail!("repository name is ambiguous; restore by repository id");
    }
    matching.sort_by(|left, right| {
        (left.manifest.generation, &left.manifest.snapshot_id)
            .cmp(&(right.manifest.generation, &right.manifest.snapshot_id))
    });

    if selected.is_some() {
        let snapshot = matching.pop().expect("matching is not empty");
        if let SnapshotHealth::Corrupt(reason) = &snapshot.health {
            bail!("selected snapshot is corrupt: {reason}");
        }
        return Ok(snapshot);
    }
    matching
        .into_iter()
        .rev()
        .find(|snapshot| snapshot.health == SnapshotHealth::Valid)
        .context("no valid snapshot found")
}

fn publish_repository(staged: &Path, destination: &Path, replace: bool) -> Result<()> {
    if !destination.exists() {
        return fs::rename(staged, destination).with_context(|| {
            format!(
                "could not publish restored repository {}",
                destination.display()
            )
        });
    }
    if !replace {
        bail!("repository already exists at {}", destination.display());
    }

    let parent = destination
        .parent()
        .context("repository destination has no parent")?;
    let replaced = parent.join(format!(".refuge-replaced-{}.git", uuid::Uuid::now_v7()));
    fs::rename(destination, &replaced).with_context(|| {
        format!(
            "could not move existing repository {} aside",
            destination.display()
        )
    })?;
    if let Err(error) = fs::rename(staged, destination) {
        let rollback = fs::rename(&replaced, destination);
        return match rollback {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "could not publish restored repository {}",
                    destination.display()
                )
            }),
            Err(rollback_error) => bail!(
                "could not publish restored repository and rollback failed: {error}; {rollback_error}"
            ),
        };
    }
    fs::remove_dir_all(&replaced).with_context(|| {
        format!(
            "could not remove replaced repository {}",
            replaced.display()
        )
    })?;
    Ok(())
}
