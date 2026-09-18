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
pub enum CatalogDiagnosticKind {
    Corrupt,
    Unsupported,
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct CatalogDiagnostic {
    pub path: PathBuf,
    pub kind: CatalogDiagnosticKind,
    pub reason: String,
}

impl CatalogDiagnostic {
    pub fn snapshot_id(&self) -> Option<&str> {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".manifest.json"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotCatalog {
    pub snapshots: Vec<Snapshot>,
    pub diagnostics: Vec<CatalogDiagnostic>,
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
    let catalog = list_for_repo(&config.target_root, repository.id)?;
    if let Some(diagnostic) = catalog.diagnostics.first() {
        return Ok(ProtectionState::Corrupt {
            reason: format!("{}: {}", diagnostic.path.display(), diagnostic.reason),
        });
    }
    let snapshots = catalog.snapshots;
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

pub fn list_snapshots(target: &Path, filter: Option<&str>) -> Result<SnapshotCatalog> {
    let root = target.join("refuge/v1/repos");
    if !root.exists() {
        return Ok(SnapshotCatalog::default());
    }
    let repo_roots = if let Some(id) = filter.and_then(|value| uuid::Uuid::parse_str(value).ok()) {
        vec![root.join(id.to_string())]
    } else {
        std::fs::read_dir(&root)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_dir())
                    .map(|_| entry.path())
            })
            .collect()
    };
    let mut catalog = SnapshotCatalog::default();
    for repo_root in repo_roots {
        if !repo_root.exists() {
            continue;
        }
        for path in manifest::paths_in(&repo_root.join("snapshots"))? {
            match manifest::read(&path) {
                Ok(manifest) => {
                    let item = inspect(&repo_root, manifest);
                    if filter.is_none_or(|value| {
                        value == item.manifest.repo_name
                            || value == item.manifest.repo_id.to_string()
                    }) {
                        catalog.snapshots.push(item);
                    }
                }
                Err(error) => catalog.diagnostics.push(diagnostic(path, &error)),
            }
        }
    }
    catalog.snapshots.sort_by(|left, right| {
        snapshot_order(&left.manifest).cmp(&snapshot_order(&right.manifest))
    });
    catalog
        .diagnostics
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(catalog)
}

fn list_for_repo(target: &Path, repo_id: uuid::Uuid) -> Result<SnapshotCatalog> {
    list_snapshots(target, Some(&repo_id.to_string()))
}

fn diagnostic(path: PathBuf, error: &anyhow::Error) -> CatalogDiagnostic {
    let kind = if let Some(validation) = error.downcast_ref::<manifest::ManifestValidationError>() {
        match validation.issue {
            manifest::ManifestIssue::Corrupt => CatalogDiagnosticKind::Corrupt,
            manifest::ManifestIssue::Unsupported => CatalogDiagnosticKind::Unsupported,
        }
    } else if error.downcast_ref::<std::io::Error>().is_some() {
        CatalogDiagnosticKind::Unreadable
    } else {
        CatalogDiagnosticKind::Corrupt
    };
    CatalogDiagnostic {
        path,
        kind,
        reason: format!("{error:#}"),
    }
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
        Ok(path) => match (
            std::fs::symlink_metadata(repo_root.join("snapshots")),
            std::fs::symlink_metadata(&path),
        ) {
            (Ok(directory), _) if !directory.file_type().is_dir() => Some(SnapshotHealth::Corrupt(
                "snapshot directory is not a real directory".to_owned(),
            )),
            (Ok(_), Ok(metadata))
                if metadata.file_type().is_file() && metadata.len() == artifact.size =>
            {
                None
            }
            (Ok(_), Ok(_)) => Some(SnapshotHealth::Corrupt(
                "artifact is not a regular file or its size differs".to_owned(),
            )),
            (Err(error), _) | (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(SnapshotHealth::Corrupt("artifact is missing".to_owned()))
            }
            (Err(error), _) | (_, Err(error)) => Some(SnapshotHealth::Corrupt(format!(
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
