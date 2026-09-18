use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use uuid::Uuid;

use crate::config::Config;
use crate::git::{self, RefState};
use crate::manifest::{self, Artifact, Manifest};
use crate::repo;

#[derive(Serialize)]
struct RepoEnvelope<'a> {
    schema_version: u32,
    repo_id: Uuid,
    name: &'a str,
    created_at: &'a str,
}

pub fn backup_named(config: &Config, name: &str) -> Result<Manifest> {
    backup_path(config, &repo::find(config, name)?)
}

pub fn backup_path(config: &Config, path: &Path) -> Result<Manifest> {
    let repo_id = Uuid::parse_str(&git::config_get(path, "refuge.repoid")?)
        .context("repository has an invalid refuge.repoid")?;
    let repo_name = repository_name(path)?;
    let repository_root = config
        .target_root
        .join("refuge/v1/repos")
        .join(repo_id.to_string());
    let snapshots = repository_root.join("snapshots");
    fs::create_dir_all(&snapshots)
        .with_context(|| format!("could not create {}", snapshots.display()))?;

    let generation = next_generation(&snapshots)?;
    let now = OffsetDateTime::now_utc();
    let created_at = now.format(&Rfc3339).context("could not format time")?;
    let compact = now
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("could not format snapshot id")?;
    let instance = config.instance_id.simple().to_string();
    let snapshot_id = format!("{compact}-g{generation}-{}", &instance[..8]);

    let staging = config.target_root.join(".refuge-staging");
    fs::create_dir_all(&staging)
        .with_context(|| format!("could not create {}", staging.display()))?;
    let staged_bundle = staging.join(format!("{snapshot_id}.bundle"));

    let (state, artifact) = create_artifact(path, &snapshots, &snapshot_id, &staged_bundle)?;
    write_repo_envelope(&repository_root, repo_id, &repo_name, &created_at)?;

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
        encryption: None,
        refuge_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let manifest_path = snapshots.join(format!("{snapshot_id}.manifest.json"));
    // The manifest is the publication marker. Nothing may make a snapshot
    // discoverable until its artifact has been durably renamed into place.
    write_json_atomic(&manifest_path, &manifest)?;
    Ok(manifest)
}

fn create_artifact(
    repo_path: &Path,
    snapshots: &Path,
    snapshot_id: &str,
    staged_bundle: &Path,
) -> Result<(RefState, Option<Artifact>)> {
    let mut state = git::ref_state(repo_path)?;
    if state.refs.is_empty() {
        return Ok((state, None));
    }

    for attempt in 0..2 {
        if staged_bundle.exists() {
            fs::remove_file(staged_bundle)?;
        }
        git::bundle_create(repo_path, staged_bundle)?;
        git::bundle_verify(repo_path, staged_bundle)?;
        if git::bundle_list_heads(staged_bundle)? == state.refs {
            let (checksum, size) = checksum(staged_bundle)?;
            let file_name = format!("{snapshot_id}.bundle");
            let destination = snapshots.join(&file_name);
            publish_file(staged_bundle, &destination)?;
            return Ok((
                state,
                Some(Artifact {
                    key: format!("snapshots/{file_name}"),
                    size,
                    checksum,
                    format: "git-bundle".to_owned(),
                    format_version: 2,
                }),
            ));
        }
        if attempt == 0 {
            state = git::ref_state(repo_path)?;
        }
    }
    let _ = fs::remove_file(staged_bundle);
    bail!("repository refs changed while creating the bundle; retry the backup")
}

fn publish_file(source: &Path, destination: &Path) -> Result<()> {
    let partial = destination.with_extension("bundle.partial");
    if partial.exists() {
        fs::remove_file(&partial)?;
    }
    fs::copy(source, &partial).with_context(|| {
        format!(
            "could not stage artifact {} at {}",
            source.display(),
            partial.display()
        )
    })?;
    File::open(&partial)?.sync_all()?;
    fs::rename(&partial, destination)
        .with_context(|| format!("could not publish artifact {}", destination.display()))?;
    fs::remove_file(source)?;
    Ok(())
}

pub(crate) fn checksum(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((format!("sha256:{:x}", digest.finalize()), size))
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

fn write_repo_envelope(root: &Path, id: Uuid, name: &str, created_at: &str) -> Result<()> {
    let destination = root.join("repo.json");
    if destination.exists() {
        return Ok(());
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

fn write_json_atomic<T: Serialize>(destination: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let partial = destination.with_extension(format!(
        "{}.partial",
        destination
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));
    let mut file = File::create(&partial)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, destination)
        .with_context(|| format!("could not publish {}", destination.display()))
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
