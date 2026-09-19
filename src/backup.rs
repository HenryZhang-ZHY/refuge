use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, bail};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::catalog::{Catalog, Health};
use crate::config::Config;
use crate::git::{self, BatchCheck, RefState};
use crate::layout::{self, RepoLayout};
use crate::manifest::{self, Artifact, GitSection, LfsSection, Manifest};
use crate::store::{self, Store};
use crate::{lfs, repo};

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupOptions {
    pub checkpoint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    Empty,
    Checkpoint,
    Delta,
    RefsOnly,
}

#[allow(clippy::large_enum_variant)] // Public shape is fixed by the v2 format plan.
pub enum BackupOutcome {
    AlreadyProtected {
        snapshot_id: String,
    },
    Published {
        manifest: Manifest,
        kind: SnapshotKind,
        git_bytes_written: u64,
        lfs_bytes_written: u64,
        lfs_objects_written: u64,
        warnings: Vec<String>,
    },
}

pub fn backup_named(config: &Config, name: &str, options: BackupOptions) -> Result<BackupOutcome> {
    backup_path(config, &repo::find(config, name)?, options)
}

pub fn backup_path(
    config: &Config,
    repository: &Path,
    options: BackupOptions,
) -> Result<BackupOutcome> {
    backup_path_at(config, repository, options, OffsetDateTime::now_utc())
}

fn backup_path_at(
    config: &Config,
    repository: &Path,
    options: BackupOptions,
    now: OffsetDateTime,
) -> Result<BackupOutcome> {
    let repo_id = Uuid::parse_str(&git::config_get(repository, "refuge.repoid")?)
        .context("repository has an invalid refuge.repoid")?;
    let file_name = repository
        .file_name()
        .and_then(|name| name.to_str())
        .context("repository path has no valid UTF-8 name")?;
    let repo_name = file_name
        .strip_suffix(".git")
        .unwrap_or(file_name)
        .to_owned();
    let locks = layout::locks_dir(config);
    fs::create_dir_all(&locks)?;
    let lock_path = locks.join(format!("{repo_id}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open lock file {}", lock_path.display()))?;
    fs2::FileExt::lock_exclusive(&lock).context("could not lock repository backup")?;
    let staging = layout::staging_dir(config);
    fs::create_dir_all(&staging)?;
    let operation = tempfile::Builder::new()
        .prefix(&format!("{repo_id}-"))
        .tempdir_in(&staging)?;
    let layout = RepoLayout::new(&config.target_root, repo_id);
    let store = Store::new(layout.clone());
    let mut warnings = store.sweep_partials()?;

    for attempt in 0..2 {
        let state = git::ref_state(repository)?;
        let catalog = Catalog::load(layout.clone())?;
        let newest = catalog.newest();
        if let Some(newest) = newest
            && newest.instance_id != config.instance_id
        {
            warnings.push(format!(
                "newest snapshot {} was published by another Refuge instance",
                newest.snapshot_id
            ));
        }
        if !options.checkpoint
            && let Some(newest) = newest
            && catalog.health(newest) == Health::Valid
            && newest.ref_state_hash == state.hash()
        {
            return Ok(BackupOutcome::AlreadyProtected {
                snapshot_id: newest.snapshot_id.clone(),
            });
        }
        let generation = catalog
            .max_generation_from_manifests()
            .max(catalog.max_generation_from_file_names())
            .checked_add(1)
            .context("snapshot generation overflow")?;
        let snapshot_id = layout::snapshot_id(now, generation, config.instance_id);
        let (parent, exclusions) = choose_parent(repository, &catalog, newest, options.checkpoint)?;
        let staged_bundle = operation.path().join("snapshot.bundle");
        let (kind, bundle) = if state.refs.is_empty() {
            (SnapshotKind::Empty, None)
        } else if parent.is_none() {
            create_bundle(repository, &staged_bundle, &snapshot_id, &state, &[])?;
            (
                SnapshotKind::Checkpoint,
                Some(bundle_artifact(&staged_bundle, &snapshot_id)?),
            )
        } else if !git::has_objects_outside(repository, &exclusions)? {
            (SnapshotKind::RefsOnly, None)
        } else {
            create_bundle(
                repository,
                &staged_bundle,
                &snapshot_id,
                &state,
                &exclusions,
            )?;
            (
                SnapshotKind::Delta,
                Some(bundle_artifact(&staged_bundle, &snapshot_id)?),
            )
        };
        if git::ref_state(repository)? != state {
            let _ = fs::remove_file(&staged_bundle);
            if attempt == 0 {
                continue;
            }
            bail!("repository refs changed while creating the bundle; retry the backup");
        }

        let set = lfs::required_set(repository)?;
        if git::ref_state(repository)? != state {
            let _ = fs::remove_file(&staged_bundle);
            if attempt == 0 {
                continue;
            }
            bail!("repository refs changed while creating the bundle; retry the backup");
        }
        let (set_bytes, lfs_section) = if set.is_empty() {
            (Vec::new(), None)
        } else {
            let (bytes, artifact) = lfs::encode_set(&set);
            let size = set
                .values()
                .try_fold(0u64, |sum, value| sum.checked_add(*value))
                .context("LFS size overflow")?;
            (
                bytes,
                Some(LfsSection {
                    set: artifact,
                    count: set.len() as u64,
                    size,
                }),
            )
        };
        let mut publish_objects = Vec::new();
        for (oid, size) in &set {
            let key = RepoLayout::lfs_object_key(oid);
            match store.stat(&key)? {
                Some(actual) if actual == *size => {}
                Some(actual) => bail!(
                    "target LFS object {oid} has size {actual}, expected {size}; the backup target is damaged — move the file aside and retry"
                ),
                None => {
                    let path = lfs::local_object_path(repository, oid);
                    let metadata = fs::symlink_metadata(&path)
                        .with_context(|| format!("required LFS object {oid} is missing"))?;
                    if !metadata.file_type().is_file() || metadata.len() != *size {
                        bail!("required LFS object {oid} is missing");
                    }
                    publish_objects.push((oid.clone(), *size, path));
                }
            }
        }
        let manifest = Manifest {
            schema_version: 2,
            repo_id,
            repo_name: repo_name.clone(),
            instance_id: config.instance_id,
            snapshot_id: snapshot_id.clone(),
            created_at: now.format(&Rfc3339)?,
            generation,
            refuge_version: env!("CARGO_PKG_VERSION").into(),
            ref_state_hash: state.hash(),
            refs: Manifest::refs_from(&state),
            git: GitSection {
                parent: parent.map(|value| value.snapshot_id.clone()),
                bundle: bundle.clone(),
            },
            lfs: lfs_section.clone(),
        };
        manifest::validate(&layout.manifest_path(&snapshot_id), &manifest)?;

        let mut lfs_bytes_written = 0;
        let mut lfs_objects_written = 0;
        for (oid, size, path) in publish_objects {
            let artifact = Artifact {
                key: RepoLayout::lfs_object_key(&oid),
                size,
                checksum: format!("sha256:{oid}"),
            };
            let written = store.publish_file(&path, &artifact.key, &artifact)?;
            lfs_bytes_written += written.bytes_written;
            lfs_objects_written += 1;
            warnings.extend(written.warnings);
        }
        if let Some(section) = &lfs_section {
            match store.stat(&section.set.key)? {
                None => {
                    let written = store.publish_bytes(&set_bytes, &section.set.key)?;
                    lfs_bytes_written += written.bytes_written;
                    warnings.extend(written.warnings);
                }
                Some(size) if size == section.set.size => {}
                Some(size) => bail!(
                    "target LFS set {} has size {size}, expected {}; the backup target is damaged — move the file aside and retry",
                    section.set.key,
                    section.set.size
                ),
            }
        }
        let mut git_bytes_written = 0;
        if let Some(bundle) = &bundle {
            if store.stat(&bundle.key)?.is_some() {
                store.remove(&bundle.key)?;
            }
            let written = store.publish_file(&staged_bundle, &bundle.key, bundle)?;
            git_bytes_written = written.bytes_written;
            warnings.extend(written.warnings);
        }
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        let manifest_key = format!("snapshots/{snapshot_id}.json");
        match store.publish_bytes(&bytes, &manifest_key) {
            Ok(written) => warnings.extend(written.warnings),
            Err(error) => {
                if let Some(bundle) = &bundle {
                    let _ = store.remove(&bundle.key);
                }
                return Err(error);
            }
        }
        return Ok(BackupOutcome::Published {
            manifest,
            kind,
            git_bytes_written,
            lfs_bytes_written,
            lfs_objects_written,
            warnings,
        });
    }
    unreachable!()
}

fn create_bundle(
    repository: &Path,
    destination: &Path,
    _snapshot_id: &str,
    state: &RefState,
    exclusions: &[String],
) -> Result<()> {
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    git::bundle_create(repository, destination, exclusions)?;
    git::bundle_verify(repository, destination)?;
    for (name, oid) in git::bundle_list_heads(destination)? {
        if state.refs.get(&name) != Some(&oid) {
            bail!("bundle ref {name} differs from captured repository state");
        }
    }
    Ok(())
}

fn bundle_artifact(path: &Path, snapshot_id: &str) -> Result<Artifact> {
    let (checksum, size) = store::sha256_file(path)?;
    Ok(Artifact {
        key: RepoLayout::bundle_key(snapshot_id),
        size,
        checksum,
    })
}

fn choose_parent<'a>(
    repository: &Path,
    catalog: &'a Catalog,
    newest: Option<&'a Manifest>,
    force: bool,
) -> Result<(Option<&'a Manifest>, Vec<String>)> {
    let Some(newest) = newest else {
        return Ok((None, Vec::new()));
    };
    if force
        || catalog.health(newest) != Health::Valid
        || newest.object_ref_count() == 0
        || checkpoint_due(catalog, newest)
    {
        return Ok((None, Vec::new()));
    }
    let tips = newest.ref_state().refs.into_values().collect::<Vec<_>>();
    if tips.is_empty() {
        return Ok((None, Vec::new()));
    }
    let mut names = Vec::with_capacity(tips.len() * 2);
    for oid in &tips {
        names.push(oid.clone());
        names.push(format!("{oid}^{{commit}}"));
    }
    let checks = git::batch_check(repository, &names)?;
    let mut exclusions = BTreeSet::new();
    for pair in checks.chunks_exact(2) {
        if matches!(pair[0], BatchCheck::Missing) {
            return Ok((None, Vec::new()));
        }
        if let BatchCheck::Found { oid, kind, .. } = &pair[1]
            && kind == "commit"
        {
            exclusions.insert(oid.clone());
        }
    }
    if exclusions.is_empty() || exclusions.len() > git::MAX_EXCLUSIONS {
        return Ok((None, Vec::new()));
    }
    Ok((Some(newest), exclusions.into_iter().collect()))
}

fn checkpoint_due(catalog: &Catalog, newest: &Manifest) -> bool {
    let Ok(chain) = catalog.chain(newest) else {
        return true;
    };
    if chain.len() >= 256 {
        return true;
    }
    let Some(root_size) = chain
        .first()
        .and_then(|root| root.git.bundle.as_ref())
        .map(|bundle| bundle.size)
    else {
        return true;
    };
    chain
        .iter()
        .skip(1)
        .filter_map(|item| item.git.bundle.as_ref())
        .map(|bundle| bundle.size)
        .sum::<u64>()
        >= root_size
}
