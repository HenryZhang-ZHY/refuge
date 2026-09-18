use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::git;
use crate::manifest::{self, Manifest, ManifestRef};
use crate::repo::{self, HostedRepository};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotHealth {
    Valid,
    Corrupt(String),
}

impl fmt::Display for SnapshotHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Valid => formatter.write_str("valid"),
            Self::Corrupt(reason) => write!(formatter, "corrupt ({reason})"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub manifest: Manifest,
    pub health: SnapshotHealth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionState {
    Protected { snapshot_id: String },
    Pending,
    Unprotected,
    Corrupt { reason: String },
}

pub fn repository_status(
    config: &Config,
    repository: &HostedRepository,
) -> Result<ProtectionState> {
    let snapshots = list_for_repo(&config.target_root, repository.id)?;
    let Some(latest) = snapshots
        .into_iter()
        .max_by(|left, right| snapshot_order(&left.manifest).cmp(&snapshot_order(&right.manifest)))
    else {
        return Ok(ProtectionState::Unprotected);
    };
    if let SnapshotHealth::Corrupt(reason) = latest.health {
        return Ok(ProtectionState::Corrupt { reason });
    }
    let current_hash = git::ref_state(&repository.path)?.hash();
    if current_hash == latest.manifest.ref_state_hash {
        Ok(ProtectionState::Protected {
            snapshot_id: latest.manifest.snapshot_id,
        })
    } else {
        Ok(ProtectionState::Pending)
    }
}

pub fn statuses(
    config: &Config,
    selected_name: Option<&str>,
) -> Result<Vec<(HostedRepository, ProtectionState)>> {
    let repositories = if let Some(name) = selected_name {
        vec![repo::resolve(config, name)?]
    } else {
        repo::list(config)?
    };
    repositories
        .into_iter()
        .map(|repository| {
            let status = repository_status(config, &repository)?;
            Ok((repository, status))
        })
        .collect()
}

pub fn list_snapshots(target: &Path, filter: Option<&str>) -> Result<Vec<Snapshot>> {
    let root = target.join("refuge/v1/repos");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut snapshots = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let repo_root = entry?.path();
        if !repo_root.is_dir() {
            continue;
        }
        for path in manifest::paths_in(&repo_root.join("snapshots"))? {
            let item = inspect(&repo_root, manifest::read(&path)?);
            if filter.is_none_or(|value| {
                value == item.manifest.repo_name || value == item.manifest.repo_id.to_string()
            }) {
                snapshots.push(item);
            }
        }
    }
    snapshots.sort_by(|left, right| {
        snapshot_order(&left.manifest).cmp(&snapshot_order(&right.manifest))
    });
    Ok(snapshots)
}

fn list_for_repo(target: &Path, repo_id: uuid::Uuid) -> Result<Vec<Snapshot>> {
    let repo_root = target.join("refuge/v1/repos").join(repo_id.to_string());
    manifest::paths_in(&repo_root.join("snapshots"))?
        .into_iter()
        .map(|path| Ok(inspect(&repo_root, manifest::read(&path)?)))
        .collect()
}

fn inspect(repo_root: &Path, manifest: Manifest) -> Snapshot {
    if repo_root.file_name().and_then(|name| name.to_str())
        != Some(manifest.repo_id.to_string().as_str())
    {
        return Snapshot {
            manifest,
            health: SnapshotHealth::Corrupt(
                "manifest repository id differs from its directory".to_owned(),
            ),
        };
    }
    let health = snapshot_health(repo_root, &manifest);
    Snapshot { manifest, health }
}

fn snapshot_health(repo_root: &Path, manifest: &Manifest) -> SnapshotHealth {
    let primary = match &manifest.artifact {
        Some(artifact) => validate_present(repo_root, artifact),
        None => {
            let only_symbolic_head = manifest.refs.iter().all(|(name, value)| {
                name == "HEAD" && matches!(value, ManifestRef::Symbolic { .. })
            });
            if only_symbolic_head {
                None
            } else {
                Some(SnapshotHealth::Corrupt(
                    "non-empty snapshot has no artifact".to_owned(),
                ))
            }
        }
    };
    if let Some(corrupt) = primary {
        return corrupt;
    }
    if let Some(lfs) = &manifest.lfs_artifact
        && let Some(corrupt) = validate_present(repo_root, lfs)
    {
        return corrupt;
    }
    SnapshotHealth::Valid
}

/// Checks one artifact for presence/size on disk. Returns `None` when it is
/// present and valid, or `Some(Corrupt(..))` describing the problem.
fn validate_present(repo_root: &Path, artifact: &manifest::Artifact) -> Option<SnapshotHealth> {
    match artifact_path_from_repo(repo_root, &artifact.key) {
        Ok(path) => match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() == artifact.size => None,
            Ok(_) => Some(SnapshotHealth::Corrupt("artifact size differs".to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(SnapshotHealth::Corrupt("artifact is missing".to_owned()))
            }
            Err(error) => Some(SnapshotHealth::Corrupt(format!(
                "artifact cannot be read: {error}"
            ))),
        },
        Err(error) => Some(SnapshotHealth::Corrupt(error.to_string())),
    }
}

pub fn artifact_path(target: &Path, manifest: &Manifest) -> Result<Option<PathBuf>> {
    let Some(artifact) = &manifest.artifact else {
        return Ok(None);
    };
    let repo_root = target
        .join("refuge/v1/repos")
        .join(manifest.repo_id.to_string());
    artifact_path_from_repo(&repo_root, &artifact.key).map(Some)
}

pub fn lfs_artifact_path(target: &Path, manifest: &Manifest) -> Result<Option<PathBuf>> {
    let Some(artifact) = &manifest.lfs_artifact else {
        return Ok(None);
    };
    let repo_root = target
        .join("refuge/v1/repos")
        .join(manifest.repo_id.to_string());
    artifact_path_from_repo(&repo_root, &artifact.key).map(Some)
}

fn artifact_path_from_repo(repo_root: &Path, key: &str) -> Result<PathBuf> {
    let key = Path::new(key);
    let mut components = key.components();
    if components.next() != Some(Component::Normal("snapshots".as_ref()))
        || !components
            .clone()
            .all(|part| matches!(part, Component::Normal(_)))
        || components.next().is_none()
    {
        bail!("artifact key is outside the repository snapshot directory");
    }
    Ok(repo_root.join(key))
}

fn snapshot_order(manifest: &Manifest) -> (u64, &str) {
    (manifest.generation, &manifest.snapshot_id)
}
