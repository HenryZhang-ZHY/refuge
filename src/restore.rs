use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::discovery::{self, Snapshot, SnapshotHealth};
use crate::storage;
use crate::{git, lfs, repo};

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
    pub skipped_candidates: Vec<String>,
}

pub fn restore(config: &Config, options: RestoreOptions<'_>) -> Result<RestoredRepository> {
    let catalog = discovery::list_snapshots(options.target, Some(options.selector))?;
    let (candidates, mut skipped_candidates) = candidates(catalog, options.snapshot_id)?;
    let first = candidates.first().context("no matching snapshot found")?;
    let name = options
        .as_name
        .unwrap_or(&first.manifest.repo_name)
        .to_owned();
    let destination = repo::destination(config, &name)?;
    if destination.exists() && !options.replace {
        bail!(
            "repository already exists at {}; pass --replace to replace it",
            destination.display()
        );
    }

    let explicit = options.snapshot_id.is_some();
    for snapshot in candidates {
        if let SnapshotHealth::Corrupt(reason) = &snapshot.health {
            let message = format!("{}: {reason}", snapshot.manifest.snapshot_id);
            if explicit {
                bail!("selected snapshot is corrupt: {reason}");
            }
            skipped_candidates.push(message);
            continue;
        }
        match prepare_snapshot(config, options.target, snapshot) {
            Ok(prepared) => {
                repo::ensure_identity_available(config, prepared.repo_id, &destination)?;
                publish_repository(&prepared.repository, &destination, options.replace)?;
                return Ok(RestoredRepository {
                    name,
                    path: destination,
                    snapshot_id: prepared.snapshot_id,
                    skipped_candidates,
                });
            }
            Err(CandidateFailure::Invalid(reason)) if !explicit => {
                skipped_candidates.push(reason);
            }
            Err(CandidateFailure::Invalid(reason)) => {
                bail!("selected snapshot is corrupt: {reason}")
            }
            Err(CandidateFailure::Fatal(error)) => return Err(error),
        }
    }
    if skipped_candidates.is_empty() {
        bail!("no valid snapshot found");
    }
    bail!(
        "no valid snapshot found; rejected candidates: {}",
        skipped_candidates.join("; ")
    )
}

struct PreparedSnapshot {
    _staging: tempfile::TempDir,
    repository: PathBuf,
    snapshot_id: String,
    repo_id: uuid::Uuid,
}

enum CandidateFailure {
    Invalid(String),
    Fatal(anyhow::Error),
}

fn prepare_snapshot(
    config: &Config,
    target: &Path,
    snapshot: Snapshot,
) -> std::result::Result<PreparedSnapshot, CandidateFailure> {
    let staging = tempfile::tempdir_in(&config.repos_dir)
        .map_err(|error| CandidateFailure::Fatal(error.into()))?;
    let artifact = copy_artifact(
        target,
        &snapshot.manifest,
        false,
        staging.path().join("snapshot.bundle"),
    )?;
    let lfs_archive = copy_artifact(
        target,
        &snapshot.manifest,
        true,
        staging.path().join("snapshot.lfs.tar"),
    )?;
    let repository = staging.path().join("repository.git");
    if let Some(path) = &artifact {
        git::init_bare(&repository).map_err(CandidateFailure::Fatal)?;
        git::bundle_verify(&repository, path).map_err(|error| {
            CandidateFailure::Invalid(format!(
                "{} failed bundle verification: {error:#}",
                snapshot.manifest.snapshot_id
            ))
        })?;
        fs::remove_dir_all(&repository).map_err(|error| CandidateFailure::Fatal(error.into()))?;
        git::clone_mirror(path, &repository).map_err(|error| {
            CandidateFailure::Invalid(format!(
                "{} could not be cloned: {error:#}",
                snapshot.manifest.snapshot_id
            ))
        })?;
    } else {
        git::init_bare(&repository).map_err(CandidateFailure::Fatal)?;
    }
    if let Some(head) = snapshot.manifest.head() {
        git::set_symbolic_head(&repository, head).map_err(|error| {
            CandidateFailure::Invalid(format!(
                "{} has an invalid symbolic HEAD: {error:#}",
                snapshot.manifest.snapshot_id
            ))
        })?;
    }
    if let Some(path) = &lfs_archive {
        lfs::extract_archive(path, &repository).map_err(|error| {
            CandidateFailure::Invalid(format!(
                "{} has an invalid LFS archive: {error:#}",
                snapshot.manifest.snapshot_id
            ))
        })?;
    }
    lfs::verify_repository(&repository).map_err(|error| {
        CandidateFailure::Invalid(format!(
            "{} is missing required LFS content: {error:#}",
            snapshot.manifest.snapshot_id
        ))
    })?;
    let actual = git::ref_state(&repository).map_err(CandidateFailure::Fatal)?;
    let expected = snapshot.manifest.ref_state();
    if actual != expected || actual.hash() != snapshot.manifest.ref_state_hash {
        return Err(CandidateFailure::Invalid(format!(
            "{} bundle refs differ from its manifest",
            snapshot.manifest.snapshot_id
        )));
    }
    git::fsck(&repository).map_err(|error| {
        CandidateFailure::Invalid(format!(
            "{} failed git fsck: {error:#}",
            snapshot.manifest.snapshot_id
        ))
    })?;
    repo::configure(&repository, snapshot.manifest.repo_id).map_err(CandidateFailure::Fatal)?;
    Ok(PreparedSnapshot {
        _staging: staging,
        repository,
        snapshot_id: snapshot.manifest.snapshot_id,
        repo_id: snapshot.manifest.repo_id,
    })
}

fn copy_artifact(
    target: &Path,
    manifest: &crate::manifest::Manifest,
    lfs: bool,
    destination: PathBuf,
) -> std::result::Result<Option<PathBuf>, CandidateFailure> {
    let descriptor = if lfs {
        manifest.lfs_artifact.as_ref()
    } else {
        manifest.artifact.as_ref()
    };
    let Some(descriptor) = descriptor else {
        return Ok(None);
    };
    let source = if lfs {
        discovery::lfs_artifact_path(target, manifest)
    } else {
        discovery::artifact_path(target, manifest)
    }
    .map_err(|error| CandidateFailure::Invalid(format!("{error:#}")))?
    .expect("artifact descriptor has a path");
    if let Err(error) = fs::copy(&source, &destination) {
        if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
        ) {
            return Err(CandidateFailure::Invalid(format!(
                "{} artifact disappeared while staging: {error}",
                manifest.snapshot_id
            )));
        }
        return Err(CandidateFailure::Fatal(error.into()));
    }
    let (checksum, size) = storage::checksum(&destination).map_err(CandidateFailure::Fatal)?;
    if size != descriptor.size || checksum != descriptor.checksum {
        return Err(CandidateFailure::Invalid(format!(
            "{} artifact checksum or size differs from its manifest",
            manifest.snapshot_id
        )));
    }
    Ok(Some(destination))
}

fn candidates(
    catalog: discovery::SnapshotCatalog,
    selected: Option<&str>,
) -> Result<(Vec<Snapshot>, Vec<String>)> {
    let mut matching: Vec<_> = catalog
        .snapshots
        .into_iter()
        .filter(|snapshot| selected.is_none_or(|id| snapshot.manifest.snapshot_id == id))
        .collect();
    if matching.is_empty() {
        if let Some(id) = selected
            && let Some(diagnostic) = catalog
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.snapshot_id() == Some(id))
        {
            bail!(
                "selected snapshot is {:?}: {}",
                diagnostic.kind,
                diagnostic.reason
            );
        }
        if !catalog.diagnostics.is_empty() {
            let reasons = catalog
                .diagnostics
                .iter()
                .map(|item| format!("{}: {}", item.path.display(), item.reason))
                .collect::<Vec<_>>()
                .join("; ");
            bail!("no valid snapshot found; invalid manifests: {reasons}");
        }
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

    matching.reverse();
    if selected.is_some() {
        matching.truncate(1);
    }
    let diagnostics = catalog
        .diagnostics
        .into_iter()
        .map(|item| format!("{}: {}", item.path.display(), item.reason))
        .collect();
    Ok((matching, diagnostics))
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
