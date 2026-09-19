use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::config::Config;
use crate::layout::{RepoLayout, TARGET_ROOT};
use crate::manifest::{self, Manifest};
use crate::repo::{self, HostedRepository};
use crate::store::Store;
use crate::{git, lfs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Valid,
    Corrupt(String),
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Valid => f.write_str("valid"),
            Self::Corrupt(reason) => write!(f, "corrupt ({reason})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    Corrupt,
    Unsupported,
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub kind: DiagnosticKind,
    pub reason: String,
}

#[derive(Debug)]
pub struct Catalog {
    layout: RepoLayout,
    snapshots: BTreeMap<String, Manifest>,
    diagnostics: Vec<Diagnostic>,
    max_file_name_generation: u64,
    health: RefCell<HashMap<String, Health>>,
}

impl Catalog {
    pub fn load(layout: RepoLayout) -> Result<Self> {
        let mut catalog = Self {
            layout,
            snapshots: BTreeMap::new(),
            diagnostics: Vec::new(),
            max_file_name_generation: 0,
            health: RefCell::new(HashMap::new()),
        };
        let directory = catalog.layout.snapshots_dir();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(catalog),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read {}", directory.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            catalog.max_file_name_generation = catalog
                .max_file_name_generation
                .max(crate::layout::generation_from_file_name(&name).unwrap_or(0));
            if name.starts_with(".refuge-") && name.ends_with(".tmp") {
                continue;
            }
            if !entry.file_type()?.is_file() || !name.ends_with(".json") {
                catalog.diagnostics.push(Diagnostic {
                    path,
                    kind: DiagnosticKind::Corrupt,
                    reason: "unexpected entry in snapshots directory".into(),
                });
                continue;
            }
            match manifest::read(&path) {
                Ok(value)
                    if value.repo_id.to_string()
                        == catalog
                            .layout
                            .root()
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default() =>
                {
                    catalog.snapshots.insert(value.snapshot_id.clone(), value);
                }
                Ok(_) => catalog.diagnostics.push(Diagnostic {
                    path,
                    kind: DiagnosticKind::Corrupt,
                    reason: "manifest repository id differs from its directory".into(),
                }),
                Err(error) => catalog.diagnostics.push(diagnostic(path, &error)),
            }
        }
        catalog.diagnostics.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(catalog)
    }

    pub fn layout(&self) -> &RepoLayout {
        &self.layout
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn get(&self, id: &str) -> Option<&Manifest> {
        self.snapshots.get(id)
    }
    pub fn newest(&self) -> Option<&Manifest> {
        self.snapshots
            .values()
            .max_by_key(|m| (m.generation, &m.snapshot_id))
    }
    pub fn ordered(&self) -> Vec<&Manifest> {
        let mut values = self.snapshots.values().collect::<Vec<_>>();
        values.sort_by_key(|m| (m.generation, &m.snapshot_id));
        values
    }
    pub fn max_generation_from_manifests(&self) -> u64 {
        self.snapshots
            .values()
            .map(|m| m.generation)
            .max()
            .unwrap_or(0)
    }
    pub fn max_generation_from_file_names(&self) -> u64 {
        self.max_file_name_generation
    }

    pub fn chain<'a>(
        &'a self,
        manifest: &'a Manifest,
    ) -> std::result::Result<Vec<&'a Manifest>, String> {
        let mut chain = Vec::new();
        let mut current = manifest;
        let mut seen = HashSet::new();
        loop {
            if chain.len() >= 4096 {
                return Err("snapshot chain exceeds 4096 entries".into());
            }
            if !seen.insert(current.snapshot_id.as_str()) {
                return Err("snapshot chain contains a cycle".into());
            }
            chain.push(current);
            let Some(parent) = &current.git.parent else {
                break;
            };
            let parent = self
                .snapshots
                .get(parent)
                .ok_or_else(|| format!("parent snapshot {parent} is missing"))?;
            if parent.repo_id != current.repo_id {
                return Err(format!(
                    "parent {} belongs to another repository",
                    parent.snapshot_id
                ));
            }
            if parent.generation >= current.generation {
                return Err(format!(
                    "parent {} does not precede child {}",
                    parent.snapshot_id, current.snapshot_id
                ));
            }
            current = parent;
        }
        chain.reverse();
        Ok(chain)
    }

    pub fn health(&self, manifest: &Manifest) -> Health {
        if let Some(value) = self.health.borrow().get(&manifest.snapshot_id) {
            return value.clone();
        }
        let value = self.compute_health(manifest);
        self.health
            .borrow_mut()
            .insert(manifest.snapshot_id.clone(), value.clone());
        value
    }

    fn compute_health(&self, manifest: &Manifest) -> Health {
        let chain = match self.chain(manifest) {
            Ok(value) => value,
            Err(reason) => return Health::Corrupt(reason),
        };
        let store = Store::new(self.layout.clone());
        for snapshot in chain {
            if let Some(bundle) = &snapshot.git.bundle {
                match store.stat(&bundle.key) {
                    Ok(Some(size)) if size == bundle.size => {}
                    Ok(Some(size)) => {
                        return Health::Corrupt(format!(
                            "bundle {} has size {size}, expected {}",
                            bundle.key, bundle.size
                        ));
                    }
                    Ok(None) => {
                        return Health::Corrupt(format!("bundle {} is missing", bundle.key));
                    }
                    Err(error) => {
                        return Health::Corrupt(format!(
                            "bundle {} cannot be inspected: {error:#}",
                            bundle.key
                        ));
                    }
                }
            }
        }
        if let Some(section) = &manifest.lfs {
            match store.stat(&section.set.key) {
                Ok(Some(size)) if size == section.set.size => {}
                Ok(Some(size)) => {
                    return Health::Corrupt(format!(
                        "LFS set {} has size {size}, expected {}",
                        section.set.key, section.set.size
                    ));
                }
                Ok(None) => {
                    return Health::Corrupt(format!("LFS set {} is missing", section.set.key));
                }
                Err(error) => {
                    return Health::Corrupt(format!(
                        "LFS set {} cannot be inspected: {error:#}",
                        section.set.key
                    ));
                }
            }
            let bytes = match store.read(&section.set.key, manifest::MAX_MANIFEST_BYTES) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Health::Corrupt(format!(
                        "LFS set {} cannot be read: {error:#}",
                        section.set.key
                    ));
                }
            };
            let set = match lfs::decode_set(&bytes, &section.set.checksum) {
                Ok(set) => set,
                Err(error) => {
                    return Health::Corrupt(format!(
                        "LFS set {} is invalid: {error:#}",
                        section.set.key
                    ));
                }
            };
            let total = set.values().copied().sum::<u64>();
            if set.len() as u64 != section.count || total != section.size {
                return Health::Corrupt(format!(
                    "LFS set {} summary differs from manifest",
                    section.set.key
                ));
            }
            for (oid, expected) in set {
                let key = RepoLayout::lfs_object_key(&oid);
                match store.stat(&key) {
                    Ok(Some(size)) if size == expected => {}
                    Ok(Some(size)) => {
                        return Health::Corrupt(format!(
                            "LFS object {oid} has size {size}, expected {expected}"
                        ));
                    }
                    Ok(None) => return Health::Corrupt(format!("LFS object {oid} is missing")),
                    Err(error) => {
                        return Health::Corrupt(format!(
                            "LFS object {oid} cannot be inspected: {error:#}"
                        ));
                    }
                }
            }
        }
        Health::Valid
    }
}

fn diagnostic(path: PathBuf, error: &anyhow::Error) -> Diagnostic {
    let kind = error
        .downcast_ref::<manifest::ManifestValidationError>()
        .map_or_else(
            || {
                if error.downcast_ref::<std::io::Error>().is_some() {
                    DiagnosticKind::Unreadable
                } else {
                    DiagnosticKind::Corrupt
                }
            },
            |validation| match validation.issue {
                manifest::ManifestIssue::Corrupt => DiagnosticKind::Corrupt,
                manifest::ManifestIssue::Unsupported => DiagnosticKind::Unsupported,
            },
        );
    Diagnostic {
        path,
        kind,
        reason: format!("{error:#}"),
    }
}

pub fn list_targets(target: &Path, filter: Option<&str>) -> Result<Vec<(Uuid, Catalog)>> {
    let root = target.join(TARGET_ROOT);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let filter_id = filter.and_then(|value| Uuid::parse_str(value).ok());
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        if filter_id.is_some_and(|wanted| wanted != id) {
            continue;
        }
        let catalog = Catalog::load(RepoLayout::new(target, id))?;
        if filter.is_none_or(|wanted| {
            filter_id.is_some()
                || catalog
                    .snapshots
                    .values()
                    .any(|manifest| manifest.repo_name == wanted)
        }) {
            result.push((id, catalog));
        }
    }
    result.sort_by_key(|(id, _)| *id);
    Ok(result)
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
    let catalog = Catalog::load(RepoLayout::new(&config.target_root, repository.id))?;
    if let Some(diagnostic) = catalog.diagnostics().first() {
        return Ok(ProtectionState::Corrupt {
            reason: format!("{}: {}", diagnostic.path.display(), diagnostic.reason),
        });
    }
    let Some(newest) = catalog.newest() else {
        return Ok(ProtectionState::Unprotected);
    };
    if let Health::Corrupt(reason) = catalog.health(newest) {
        return Ok(ProtectionState::Corrupt { reason });
    }
    if newest.ref_state_hash == git::ref_state(&repository.path)?.hash() {
        Ok(ProtectionState::Protected {
            snapshot_id: newest.snapshot_id.clone(),
        })
    } else {
        Ok(ProtectionState::Pending)
    }
}

pub fn statuses(
    config: &Config,
    selector: Option<&str>,
) -> Result<Vec<(HostedRepository, ProtectionState)>> {
    let repositories = if let Some(selector) = selector {
        vec![repo::resolve(config, selector)?]
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
