use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use uuid::Uuid;

use crate::config::Config;
use crate::git::{self, RefState};
use crate::manifest::{self, Artifact, Manifest};
use crate::repo;
use crate::storage::{self, FsSnapshotStore};

#[derive(Serialize)]
struct RepoEnvelope<'a> {
    schema_version: u32,
    repo_id: Uuid,
    name: &'a str,
    created_at: &'a str,
}

pub struct BackupOutcome {
    pub manifest: Manifest,
    pub warnings: Vec<String>,
}

pub fn backup_named(config: &Config, name: &str) -> Result<BackupOutcome> {
    backup_path(config, &repo::find(config, name)?)
}

pub fn backup_path(config: &Config, path: &Path) -> Result<BackupOutcome> {
    backup_path_at(config, path, OffsetDateTime::now_utc(), || {})
}

fn backup_path_at(
    config: &Config,
    path: &Path,
    now: OffsetDateTime,
    after_bundle_created: impl Fn(),
) -> Result<BackupOutcome> {
    let repo_id = Uuid::parse_str(&git::config_get(path, "refuge.repoid")?)
        .context("repository has an invalid refuge.repoid")?;
    let repo_name = repository_name(path)?;

    let staging = config.target_root.join(".refuge-staging");
    fs::create_dir_all(&staging)
        .with_context(|| format!("could not create {}", staging.display()))?;
    let lock_path = staging.join(format!("{repo_id}.lock"));
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open lock file {}", lock_path.display()))?;
    // Generation selection, artifact publication, and manifest publication
    // form one per-repository transaction. OS locks are released on crashes.
    fs2::FileExt::lock_exclusive(&lock_file).context("could not lock repository backup")?;

    let repository_root = config
        .target_root
        .join("refuge/v1/repos")
        .join(repo_id.to_string());
    let snapshots = repository_root.join("snapshots");
    fs::create_dir_all(&snapshots)
        .with_context(|| format!("could not create {}", snapshots.display()))?;

    let generation = next_generation(&snapshots)?;
    let created_at = now.format(&Rfc3339).context("could not format time")?;
    let compact = now
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("could not format snapshot id")?;
    let instance = config.instance_id.simple().to_string();
    let snapshot_id = format!("{compact}-g{generation}-{}", &instance[..8]);

    // A snapshot id is only unique within one repository namespace. Give
    // every operation its own exclusively created directory so repositories
    // with the same generation and timestamp can never share intermediate
    // bundle or LFS paths.
    let operation = tempfile::Builder::new()
        .prefix(&format!("{repo_id}-"))
        .tempdir_in(&staging)
        .with_context(|| {
            format!(
                "could not create staging directory in {}",
                staging.display()
            )
        })?;
    let staged_bundle = operation.path().join(format!("{snapshot_id}.bundle"));

    let mut warnings = Vec::new();
    let (state, artifact, artifact_warnings) = create_artifact(
        path,
        &snapshots,
        &snapshot_id,
        &staged_bundle,
        after_bundle_created,
    )?;
    warnings.extend(artifact_warnings);
    let staged_lfs = operation.path().join(format!("{snapshot_id}.lfs.tar"));
    let (lfs_artifact, lfs_warnings) =
        create_lfs_artifact(path, &snapshots, &snapshot_id, &staged_lfs)?;
    warnings.extend(lfs_warnings);
    warnings.extend(
        write_repo_envelope(&repository_root, repo_id, &repo_name, &created_at)?
            .warnings()
            .iter()
            .cloned(),
    );

    let manifest = Manifest {
        schema_version: 1,
        repo_id,
        repo_name,
        instance_id: config.instance_id,
        snapshot_id: snapshot_id.clone(),
        created_at,
        generation,
        ref_state_hash: state.hash(),
        refs: Manifest::refs_from(&state),
        artifact,
        lfs_artifact,
        encryption: None,
        refuge_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let manifest_path = snapshots.join(format!("{snapshot_id}.manifest.json"));
    // The manifest is the publication marker. Nothing may make a snapshot
    // discoverable until its artifact has been durably renamed into place.
    warnings.extend(
        write_json_atomic(&manifest_path, &manifest)?
            .warnings()
            .iter()
            .cloned(),
    );
    Ok(BackupOutcome { manifest, warnings })
}

fn create_artifact(
    repo_path: &Path,
    snapshots: &Path,
    snapshot_id: &str,
    staged_bundle: &Path,
    after_bundle_created: impl Fn(),
) -> Result<(RefState, Option<Artifact>, Vec<String>)> {
    let mut state = git::ref_state(repo_path)?;
    if state.refs.is_empty() {
        return Ok((state, None, Vec::new()));
    }

    for attempt in 0..2 {
        if staged_bundle.exists() {
            fs::remove_file(staged_bundle)?;
        }
        git::bundle_create(repo_path, staged_bundle)?;
        after_bundle_created();
        git::bundle_verify(repo_path, staged_bundle)?;
        if git::bundle_list_heads(staged_bundle)? == state.refs {
            let (checksum, size) = storage::checksum(staged_bundle)?;
            let file_name = format!("{snapshot_id}.bundle");
            let destination = snapshots.join(&file_name);
            let outcome =
                FsSnapshotStore.publish_file(staged_bundle, &destination, &checksum, size)?;
            return Ok((
                state,
                Some(Artifact {
                    key: format!("snapshots/{file_name}"),
                    size,
                    checksum,
                    format: "git-bundle".to_owned(),
                    format_version: 2,
                }),
                outcome.warnings().to_vec(),
            ));
        }
        if attempt == 0 {
            state = git::ref_state(repo_path)?;
        }
    }
    let _ = fs::remove_file(staged_bundle);
    bail!("repository refs changed while creating the bundle; retry the backup")
}

/// Directory git-lfs uses to store LFS object content on a hosted (or
/// restored) bare repository. `git bundle` never captures this, since it
/// only knows about Git objects, so it must be snapshotted separately.
fn lfs_objects_dir(repo_path: &Path) -> PathBuf {
    repo_path.join("lfs").join("objects")
}

fn create_lfs_artifact(
    repo_path: &Path,
    snapshots: &Path,
    snapshot_id: &str,
    staged_archive: &Path,
) -> Result<(Option<Artifact>, Vec<String>)> {
    let lfs_objects = lfs_objects_dir(repo_path);
    let files: Vec<PathBuf> = if lfs_objects.is_dir() {
        walk_files(&lfs_objects)?
    } else {
        Vec::new()
    };
    if files.is_empty() {
        return Ok((None, Vec::new()));
    }
    for path in &files {
        verify_lfs_object(path)?;
    }

    if staged_archive.exists() {
        fs::remove_file(staged_archive)
            .with_context(|| format!("could not remove stale {}", staged_archive.display()))?;
    }
    {
        let file = File::create(staged_archive)
            .with_context(|| format!("could not create {}", staged_archive.display()))?;
        let mut builder = tar::Builder::new(file);
        builder
            .append_dir_all(".", &lfs_objects)
            .with_context(|| format!("could not archive {}", lfs_objects.display()))?;
        builder
            .into_inner()
            .with_context(|| format!("could not finish archive {}", staged_archive.display()))?;
    }

    let (checksum, size) = storage::checksum(staged_archive)?;
    let file_name = format!("{snapshot_id}.lfs.tar");
    let destination = snapshots.join(&file_name);
    let outcome = FsSnapshotStore.publish_file(staged_archive, &destination, &checksum, size)?;
    Ok((
        Some(Artifact {
            key: format!("snapshots/{file_name}"),
            size,
            checksum,
            format: "lfs-archive".to_owned(),
            format_version: 1,
        }),
        outcome.warnings().to_vec(),
    ))
}

/// LFS object files are named after their own content hash (`sha256:<oid>`
/// is the filename), regardless of the two levels of shard directories they
/// sit under. Recomputing and comparing the hash catches local corruption
/// before it is durably archived (or, on restore, right after extraction).
fn verify_lfs_object(path: &Path) -> Result<()> {
    let oid = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("LFS object path is not valid UTF-8: {}", path.display()))?;
    let (actual, _) = storage::checksum(path)?;
    let expected = format!("sha256:{oid}");
    if actual != expected {
        bail!(
            "LFS object {} is corrupt: content hash does not match its object id",
            path.display()
        );
    }
    Ok(())
}

/// Verifies every object under an `lfs/objects` directory (if it exists)
/// against its own filename-derived content hash. Used both before
/// archiving a snapshot and after extracting one during restore.
pub fn verify_lfs_objects(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for path in walk_files(dir)? {
        verify_lfs_object(&path)?;
    }
    Ok(())
}

fn walk_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)
            .with_context(|| format!("could not read directory {}", current.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn next_generation(snapshots: &Path) -> Result<u64> {
    let mut generation = 0;
    for path in manifest::paths_in(snapshots)? {
        let manifest = manifest::read(&path)?;
        generation = generation.max(manifest.generation);
    }
    generation
        .checked_add(1)
        .context("snapshot generation overflow")
}

fn write_repo_envelope(
    root: &Path,
    id: Uuid,
    name: &str,
    created_at: &str,
) -> Result<storage::CommitOutcome> {
    let destination = root.join("repo.json");
    if destination.exists() {
        return Ok(storage::CommitOutcome::Committed);
    }
    write_json_atomic(
        &destination,
        &RepoEnvelope {
            schema_version: 1,
            repo_id: id,
            name,
            created_at,
        },
    )
}

fn write_json_atomic<T: Serialize>(
    destination: &Path,
    value: &T,
) -> Result<storage::CommitOutcome> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    FsSnapshotStore.publish_bytes(&bytes, destination)
}

fn repository_name(path: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("repository path has no valid UTF-8 name")?;
    Ok(file_name
        .strip_suffix(".git")
        .unwrap_or(file_name)
        .to_owned())
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::{Arc, Barrier};

    use super::*;

    fn repository(root: &Path, name: &str, id: Uuid, content: &str) -> PathBuf {
        let path = root.join(format!("{name}.git"));
        git::init_bare(&path).unwrap();
        git::config_set(&path, "refuge.repoid", &id.to_string()).unwrap();

        let input = root.join(format!("{name}.txt"));
        fs::write(&input, content).unwrap();
        let output = Command::new("git")
            .args(["-c", "safe.bareRepository=all"])
            .arg("-C")
            .arg(&path)
            .args(["hash-object", "-w"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(output.stdout).unwrap();
        let output = Command::new("git")
            .args(["-c", "safe.bareRepository=all"])
            .arg("-C")
            .arg(&path)
            .args(["update-ref", "refs/notes/snapshot", oid.trim()])
            .output()
            .unwrap();
        assert!(output.status.success());
        path
    }

    #[test]
    fn simultaneous_repositories_with_the_same_snapshot_id_use_isolated_staging() {
        let temp = tempfile::tempdir().unwrap();
        let repos = temp.path().join("repos");
        let target = temp.path().join("target");
        fs::create_dir_all(&repos).unwrap();
        fs::create_dir_all(&target).unwrap();
        let config = Config {
            repos_dir: repos.clone(),
            target_root: target.clone(),
            instance_id: Uuid::nil(),
        };
        let first = repository(&repos, "first", Uuid::now_v7(), "first repository");
        let second = repository(&repos, "second", Uuid::now_v7(), "second repository");
        let barrier = Arc::new(Barrier::new(2));
        let now = OffsetDateTime::UNIX_EPOCH;

        let handles: Vec<_> = [first, second]
            .into_iter()
            .map(|path| {
                let config = config.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    backup_path_at(&config, &path, now, || {
                        barrier.wait();
                    })
                })
            })
            .collect();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();

        assert_eq!(
            outcomes[0].manifest.snapshot_id,
            outcomes[1].manifest.snapshot_id
        );
        for outcome in outcomes {
            let manifest = outcome.manifest;
            let expected_refs = manifest
                .refs
                .iter()
                .filter_map(|(name, value)| match value {
                    crate::manifest::ManifestRef::Object(oid) => Some((name.clone(), oid.clone())),
                    crate::manifest::ManifestRef::Symbolic { .. } => None,
                })
                .collect();
            let artifact = manifest.artifact.unwrap();
            let bundle = target
                .join("refuge/v1/repos")
                .join(manifest.repo_id.to_string())
                .join(artifact.key);
            assert_eq!(git::bundle_list_heads(&bundle).unwrap(), expected_refs);
        }

        let staging = target.join(".refuge-staging");
        assert!(
            fs::read_dir(staging)
                .unwrap()
                .all(|entry| entry.unwrap().path().is_file())
        );
    }
}
