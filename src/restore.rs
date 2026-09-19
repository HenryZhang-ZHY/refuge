use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::catalog::{self, Catalog, Health};
use crate::config::Config;
use crate::layout::RepoLayout;
use crate::manifest::{Artifact, Manifest};
use crate::store::{CopyError, Store};
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
    pub warnings: Vec<String>,
}

pub(crate) enum CandidateFailure {
    Invalid(String),
    Fatal(anyhow::Error),
}

pub fn restore(config: &Config, options: RestoreOptions<'_>) -> Result<RestoredRepository> {
    let mut warnings = recover_replacements(&config.repos_dir)?;
    let mut targets = catalog::list_targets(options.target, Some(options.selector))?;
    if targets.is_empty() {
        bail!("no matching snapshot found");
    }
    if targets.len() > 1 {
        bail!("repository name is ambiguous; restore by repository id");
    }
    let (_, catalog) = targets.pop().unwrap();
    let mut candidates = catalog.ordered();
    candidates.reverse();
    if let Some(id) = options.snapshot_id {
        candidates.retain(|manifest| manifest.snapshot_id == id);
        if candidates.is_empty() {
            if let Some(diagnostic) = catalog
                .diagnostics()
                .iter()
                .find(|item| item.path.file_stem().and_then(|stem| stem.to_str()) == Some(id))
            {
                bail!(
                    "selected snapshot is {:?}: {}",
                    diagnostic.kind,
                    diagnostic.reason
                );
            }
            bail!("selected snapshot was not found");
        }
    }
    let first = candidates.first().context("no matching snapshot found")?;
    let name = options.as_name.unwrap_or(&first.repo_name).to_owned();
    let destination = repo::destination(config, &name)?;
    let _name_lock = repo::lock_name(config, &name)?;
    if destination.exists() && !options.replace {
        bail!(
            "repository already exists at {}; pass --replace to replace it",
            destination.display()
        );
    }
    let explicit = options.snapshot_id.is_some();
    let mut skipped = catalog
        .diagnostics()
        .iter()
        .map(|item| format!("{}: {}", item.path.display(), item.reason))
        .collect::<Vec<_>>();
    for manifest in candidates {
        if let Health::Corrupt(reason) = catalog.health(manifest) {
            if explicit {
                bail!("selected snapshot is corrupt: {reason}");
            }
            skipped.push(format!("{}: {reason}", manifest.snapshot_id));
            continue;
        }
        let staging = tempfile::Builder::new()
            .prefix(".refuge-restore-")
            .tempdir_in(&config.repos_dir)?;
        match materialize(
            &Store::new(catalog.layout().clone()),
            &catalog,
            manifest,
            staging.path(),
        ) {
            Ok(()) => {
                let repository = staging.path().join("repository.git");
                let _identity_lock = repo::lock_identity(config, manifest.repo_id)?;
                repo::ensure_identity_available(config, manifest.repo_id, &destination)?;
                warnings.extend(publish_repository(
                    &repository,
                    &destination,
                    options.replace,
                )?);
                return Ok(RestoredRepository {
                    name,
                    path: destination,
                    snapshot_id: manifest.snapshot_id.clone(),
                    skipped_candidates: skipped,
                    warnings,
                });
            }
            Err(CandidateFailure::Invalid(reason)) if explicit => {
                bail!("selected snapshot is corrupt: {reason}")
            }
            Err(CandidateFailure::Invalid(reason)) => {
                skipped.push(format!("{}: {reason}", manifest.snapshot_id))
            }
            Err(CandidateFailure::Fatal(error)) => return Err(error),
        }
    }
    bail!(
        "no valid snapshot found; rejected candidates: {}",
        skipped.join("; ")
    )
}

pub(crate) fn materialize(
    store: &Store,
    catalog: &Catalog,
    manifest: &Manifest,
    staging: &Path,
) -> std::result::Result<(), CandidateFailure> {
    let chain = catalog.chain(manifest).map_err(CandidateFailure::Invalid)?;
    let bundles = staging.join("bundles");
    fs::create_dir_all(&bundles).map_err(|error| CandidateFailure::Fatal(error.into()))?;
    let mut copied = Vec::new();
    for (index, snapshot) in chain.iter().enumerate() {
        if let Some(bundle) = &snapshot.git.bundle {
            let path = bundles.join(format!("{index}.bundle"));
            store
                .copy_out(&bundle.key, &path, bundle)
                .map_err(copy_failure)?;
            copied.push((snapshot.snapshot_id.as_str(), bundle.key.as_str(), path));
        }
    }
    let repository = staging.join("repository.git");
    git::init_bare(&repository).map_err(CandidateFailure::Fatal)?;
    git::config_set(&repository, "gc.auto", "0").map_err(CandidateFailure::Fatal)?;
    git::config_set(&repository, "maintenance.auto", "false").map_err(CandidateFailure::Fatal)?;
    for (snapshot, key, path) in copied {
        git::bundle_unbundle(&repository, &path).map_err(|error| {
            CandidateFailure::Invalid(format!(
                "{snapshot} bundle {key} failed to unbundle: {error:#}"
            ))
        })?;
    }
    git::create_refs(&repository, &manifest.ref_state().refs).map_err(|error| {
        CandidateFailure::Invalid(format!("refs could not be created: {error:#}"))
    })?;
    if let Some(head) = manifest.head() {
        git::set_symbolic_head(&repository, head).map_err(|error| {
            CandidateFailure::Invalid(format!("symbolic HEAD is invalid: {error:#}"))
        })?;
    }
    let expected_set = if let Some(section) = &manifest.lfs {
        let bytes = store
            .read(&section.set.key, crate::manifest::MAX_MANIFEST_BYTES)
            .map_err(|error| {
                CandidateFailure::Invalid(format!("LFS set could not be read: {error:#}"))
            })?;
        let set = lfs::decode_set(&bytes, &section.set.checksum)
            .map_err(|error| CandidateFailure::Invalid(format!("LFS set is invalid: {error:#}")))?;
        for (oid, size) in &set {
            let artifact = Artifact {
                key: RepoLayout::lfs_object_key(oid),
                size: *size,
                checksum: format!("sha256:{oid}"),
            };
            store
                .copy_out(
                    &artifact.key,
                    &lfs::local_object_path(&repository, oid),
                    &artifact,
                )
                .map_err(copy_failure)?;
        }
        set
    } else {
        Default::default()
    };
    let actual = git::ref_state(&repository).map_err(CandidateFailure::Fatal)?;
    if actual != manifest.ref_state() || actual.hash() != manifest.ref_state_hash {
        return Err(CandidateFailure::Invalid(
            "refs differ from manifest".into(),
        ));
    }
    git::fsck(&repository)
        .map_err(|error| CandidateFailure::Invalid(format!("git fsck failed: {error:#}")))?;
    let actual_set = lfs::required_set(&repository).map_err(|error| {
        CandidateFailure::Invalid(format!("could not derive LFS set: {error:#}"))
    })?;
    if actual_set != expected_set {
        return Err(CandidateFailure::Invalid(
            "LFS set differs from history".into(),
        ));
    }
    repo::configure(&repository, manifest.repo_id).map_err(CandidateFailure::Fatal)?;
    Ok(())
}

fn copy_failure(error: CopyError) -> CandidateFailure {
    match error {
        CopyError::Missing(reason) | CopyError::Mismatch(reason) => {
            CandidateFailure::Invalid(reason)
        }
        CopyError::Io(error) => CandidateFailure::Fatal(error),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryRecord {
    destination_name: String,
}
fn recovery_root(repos_dir: &Path) -> PathBuf {
    repos_dir.join(".refuge-recovery")
}

fn recover_replacements(repos_dir: &Path) -> Result<Vec<String>> {
    let root = recovery_root(repos_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut warnings = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let operation = entry.path();
        let record: RecoveryRecord =
            serde_json::from_slice(&fs::read(operation.join("record.json")).with_context(
                || format!("could not read recovery record in {}", operation.display()),
            )?)
            .with_context(|| format!("invalid recovery record in {}", operation.display()))?;
        let destination_name = Path::new(&record.destination_name);
        if destination_name.components().count() != 1
            || destination_name.file_name().is_none()
            || !record.destination_name.ends_with(".git")
        {
            bail!("unsafe repository replacement recovery record");
        }
        let destination = repos_dir.join(destination_name);
        let previous = operation.join("previous.git");
        if destination.exists() {
            if previous.exists() {
                warnings.push(format!(
                    "completed cleanup of a previously committed replacement for {}",
                    destination.display()
                ));
            }
            fs::remove_dir_all(&operation)?;
        } else if previous.exists() {
            fs::rename(&previous, &destination)?;
            fs::remove_dir_all(&operation)?;
            warnings.push(format!(
                "recovered an interrupted replacement at {}",
                destination.display()
            ));
        } else {
            fs::remove_dir_all(&operation)?;
        }
    }
    Ok(warnings)
}

fn publish_repository(staged: &Path, destination: &Path, replace: bool) -> Result<Vec<String>> {
    if !destination.exists() {
        fs::rename(staged, destination).with_context(|| {
            format!(
                "could not publish restored repository {}",
                destination.display()
            )
        })?;
        return Ok(Vec::new());
    }
    if !replace {
        bail!("repository already exists at {}", destination.display());
    }
    let parent = destination
        .parent()
        .context("repository destination has no parent")?;
    let root = recovery_root(parent);
    fs::create_dir_all(&root)?;
    let operation = root.join(uuid::Uuid::now_v7().to_string());
    fs::create_dir(&operation)?;
    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("repository destination name is not valid UTF-8")?
        .to_owned();
    let bytes = {
        let mut bytes = serde_json::to_vec_pretty(&RecoveryRecord { destination_name })?;
        bytes.push(b'\n');
        bytes
    };
    let mut record = tempfile::Builder::new()
        .prefix(".refuge-record-")
        .tempfile_in(&operation)?;
    record.write_all(&bytes)?;
    record.as_file_mut().sync_all()?;
    record
        .persist_noclobber(operation.join("record.json"))
        .map_err(|error| error.error)?;
    let replaced = operation.join("previous.git");
    fs::rename(destination, &replaced)?;
    if let Err(error) = fs::rename(staged, destination) {
        return match fs::rename(&replaced, destination) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&operation);
                Err(error).with_context(|| {
                    format!(
                        "could not publish restored repository {}",
                        destination.display()
                    )
                })
            }
            Err(rollback) => bail!(
                "could not publish restored repository and rollback failed: {error}; {rollback}"
            ),
        };
    }
    match fs::remove_dir_all(&operation) {
        Ok(()) => Ok(Vec::new()),
        Err(error) => Ok(vec![format!(
            "repository replacement committed, but previous data remains at {}: {error}",
            operation.display()
        )]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation(repos: &Path, name: &str) -> PathBuf {
        let operation = recovery_root(repos).join(uuid::Uuid::now_v7().to_string());
        fs::create_dir_all(&operation).unwrap();
        fs::write(
            operation.join("record.json"),
            serde_json::to_vec(&RecoveryRecord {
                destination_name: name.into(),
            })
            .unwrap(),
        )
        .unwrap();
        operation
    }
    #[test]
    fn recovers_interrupted_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let repos = temp.path().join("repos");
        fs::create_dir(&repos).unwrap();
        let op = operation(&repos, "notes.git");
        fs::create_dir(op.join("previous.git")).unwrap();
        fs::write(op.join("previous.git/marker"), b"old").unwrap();
        assert!(!recover_replacements(&repos).unwrap().is_empty());
        assert_eq!(fs::read(repos.join("notes.git/marker")).unwrap(), b"old");
    }
}
