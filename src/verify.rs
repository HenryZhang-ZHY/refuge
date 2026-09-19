use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::catalog::{self, Health};
use crate::config::Config;
use crate::restore::CandidateFailure;
use crate::store::Store;

pub fn verify(
    config: &Config,
    selector: &str,
    snapshot_id: Option<&str>,
    target: &Path,
) -> Result<String> {
    let mut targets = catalog::list_targets(target, Some(selector))?;
    if targets.len() != 1 {
        bail!(if targets.is_empty() {
            "no matching snapshot found"
        } else {
            "repository name is ambiguous; verify by repository id"
        });
    }
    let (_, catalog) = targets.pop().unwrap();
    let manifest = if let Some(id) = snapshot_id {
        catalog
            .get(id)
            .with_context(|| format!("snapshot {id} was not found"))?
    } else {
        catalog.newest().context("no matching snapshot found")?
    };
    if let Health::Corrupt(reason) = catalog.health(manifest) {
        bail!("invalid {}: {reason}", manifest.snapshot_id);
    }
    let staging = tempfile::Builder::new()
        .prefix(".refuge-verify-")
        .tempdir_in(&config.repos_dir)?;
    match crate::restore::materialize(
        &Store::new(catalog.layout().clone()),
        &catalog,
        manifest,
        staging.path(),
    ) {
        Ok(()) => Ok(manifest.snapshot_id.clone()),
        Err(CandidateFailure::Invalid(reason)) => {
            bail!("invalid {}: {reason}", manifest.snapshot_id)
        }
        Err(CandidateFailure::Fatal(error)) => Err(error),
    }
}

#[derive(Debug, Default)]
pub struct Usage {
    pub repo_name: String,
    pub repo_id: uuid::Uuid,
    pub checkpoints: (u64, u64),
    pub deltas: (u64, u64),
    pub lfs_objects: (u64, u64),
    pub lfs_sets: (u64, u64),
    pub manifests: (u64, u64),
    pub partials: (u64, u64),
    pub orphans: (u64, u64),
    pub total: u64,
}

pub fn usage(target: &Path, selector: Option<&str>) -> Result<Vec<Usage>> {
    let mut result = Vec::new();
    for (repo_id, catalog) in catalog::list_targets(target, selector)? {
        let mut item = Usage {
            repo_id,
            repo_name: catalog
                .newest()
                .map_or_else(|| "(unknown)".into(), |m| m.repo_name.clone()),
            ..Usage::default()
        };
        let known_bundles = catalog
            .ordered()
            .iter()
            .filter_map(|manifest| {
                manifest
                    .git
                    .bundle
                    .as_ref()
                    .map(|bundle| bundle.key.as_str())
            })
            .collect::<std::collections::HashSet<_>>();
        for manifest in catalog.ordered() {
            if let Some(bundle) = &manifest.git.bundle {
                if manifest.git.parent.is_none() {
                    item.checkpoints.0 += 1;
                    item.checkpoints.1 += bundle.size;
                } else {
                    item.deltas.0 += 1;
                    item.deltas.1 += bundle.size;
                }
            }
            let path = catalog.layout().manifest_path(&manifest.snapshot_id);
            if let Ok(metadata) = std::fs::metadata(path) {
                item.manifests.0 += 1;
                item.manifests.1 += metadata.len();
            }
        }
        walk(
            catalog.layout().root(),
            catalog.layout().root(),
            &known_bundles,
            &mut item,
        )?;
        result.push(item);
    }
    Ok(result)
}

fn walk(
    root: &Path,
    directory: &Path,
    known_bundles: &std::collections::HashSet<&str>,
    usage: &mut Usage,
) -> Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            walk(root, &path, known_bundles, usage)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        usage.total += metadata.len();
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".refuge-") && name.ends_with(".tmp") {
            usage.partials.0 += 1;
            usage.partials.1 += metadata.len();
        } else if relative.starts_with("lfs/objects/") {
            usage.lfs_objects.0 += 1;
            usage.lfs_objects.1 += metadata.len();
        } else if relative.starts_with("lfs/sets/") {
            usage.lfs_sets.0 += 1;
            usage.lfs_sets.1 += metadata.len();
        } else if relative.starts_with("git/") && !known_bundles.contains(relative.as_str()) {
            usage.orphans.0 += 1;
            usage.orphans.1 += metadata.len();
        }
    }
    Ok(())
}
