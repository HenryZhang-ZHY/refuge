use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::git::RefState;

pub const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestIssue {
    Corrupt,
    Unsupported,
}

#[derive(Debug)]
pub struct ManifestValidationError {
    pub issue: ManifestIssue,
    message: String,
}

impl std::fmt::Display for ManifestValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ManifestValidationError {}

fn issue(kind: ManifestIssue, message: impl Into<String>) -> anyhow::Error {
    ManifestValidationError {
        issue: kind,
        message: message.into(),
    }
    .into()
}
fn corrupt(message: impl Into<String>) -> anyhow::Error {
    issue(ManifestIssue::Corrupt, message)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestRef {
    Object(String),
    Symbolic { symref: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub key: String,
    pub size: u64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSection {
    pub parent: Option<String>,
    pub bundle: Option<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LfsSection {
    pub set: Artifact,
    pub count: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub repo_id: Uuid,
    pub repo_name: String,
    pub instance_id: Uuid,
    pub snapshot_id: String,
    pub created_at: String,
    pub generation: u64,
    pub refuge_version: String,
    pub ref_state_hash: String,
    pub refs: BTreeMap<String, ManifestRef>,
    pub git: GitSection,
    pub lfs: Option<LfsSection>,
}

impl Manifest {
    pub fn refs_from(state: &RefState) -> BTreeMap<String, ManifestRef> {
        let mut refs = state
            .refs
            .iter()
            .map(|(name, oid)| (name.clone(), ManifestRef::Object(oid.clone())))
            .collect::<BTreeMap<_, _>>();
        if let Some(head) = &state.head {
            refs.insert(
                "HEAD".into(),
                ManifestRef::Symbolic {
                    symref: head.clone(),
                },
            );
        }
        refs
    }
    pub fn head(&self) -> Option<&str> {
        match self.refs.get("HEAD") {
            Some(ManifestRef::Symbolic { symref }) => Some(symref),
            _ => None,
        }
    }
    pub fn ref_state(&self) -> RefState {
        RefState {
            refs: self
                .refs
                .iter()
                .filter_map(|(name, value)| match value {
                    ManifestRef::Object(oid) => Some((name.clone(), oid.clone())),
                    ManifestRef::Symbolic { .. } => None,
                })
                .collect(),
            head: self.head().map(str::to_owned),
        }
    }
    pub fn object_ref_count(&self) -> usize {
        self.refs
            .values()
            .filter(|value| matches!(value, ManifestRef::Object(_)))
            .count()
    }
}

pub fn read(path: &Path) -> anyhow::Result<Manifest> {
    let file =
        File::open(path).with_context(|| format!("could not open manifest {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read manifest {}", path.display()))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(corrupt(format!(
            "manifest {} exceeds the {} byte limit",
            path.display(),
            MAX_MANIFEST_BYTES
        )));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid manifest {}", path.display()))?;
    validate(path, &manifest)?;
    Ok(manifest)
}

pub fn validate(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    if manifest.schema_version != 2 {
        return Err(issue(
            ManifestIssue::Unsupported,
            format!(
                "unsupported manifest schema version {}",
                manifest.schema_version
            ),
        ));
    }
    if !valid_snapshot_id(&manifest.snapshot_id) {
        return Err(corrupt(
            "manifest snapshot id contains invalid path characters",
        ));
    }
    if path.file_name().and_then(|name| name.to_str())
        != Some(format!("{}.json", manifest.snapshot_id).as_str())
    {
        return Err(corrupt("manifest snapshot id differs from its filename"));
    }
    if manifest.generation == 0 {
        return Err(corrupt("manifest generation must be at least one"));
    }
    validate_refs(manifest)?;
    if let Some(parent) = &manifest.git.parent
        && (!valid_snapshot_id(parent) || parent == &manifest.snapshot_id)
    {
        return Err(corrupt("manifest has an invalid Git parent"));
    }
    if let Some(bundle) = &manifest.git.bundle {
        if bundle.key != crate::layout::RepoLayout::bundle_key(&manifest.snapshot_id) {
            return Err(corrupt("bundle key does not match the snapshot layout"));
        }
        checksum_digest(&bundle.checksum)?;
    }
    let has_objects = manifest.object_ref_count() != 0;
    if manifest.git.parent.is_none() && has_objects && manifest.git.bundle.is_none() {
        return Err(corrupt("non-empty checkpoint has no Git bundle"));
    }
    if !has_objects && (manifest.git.bundle.is_some() || manifest.lfs.is_some()) {
        return Err(corrupt("empty snapshot contains an artifact"));
    }
    if let Some(lfs) = &manifest.lfs {
        if lfs.count == 0 {
            return Err(corrupt("LFS section has an empty set"));
        }
        let digest = checksum_digest(&lfs.set.checksum)?;
        if lfs.set.key != crate::layout::RepoLayout::lfs_set_key(digest) {
            return Err(corrupt("LFS set key does not match its checksum"));
        }
    }
    Ok(())
}

fn validate_refs(manifest: &Manifest) -> anyhow::Result<()> {
    let mut refs = BTreeMap::new();
    let mut head = None;
    for (name, value) in &manifest.refs {
        match (name.as_str(), value) {
            ("HEAD", ManifestRef::Symbolic { symref }) if valid_ref_name(symref) => {
                head = Some(symref.clone())
            }
            ("HEAD", _) => return Err(corrupt("manifest HEAD must be a valid symbolic ref")),
            (_, ManifestRef::Object(oid)) if valid_ref_name(name) && valid_oid(oid) => {
                refs.insert(name.clone(), oid.clone());
            }
            (_, ManifestRef::Symbolic { .. }) => {
                return Err(corrupt("only manifest HEAD may be symbolic"));
            }
            _ => {
                return Err(corrupt(
                    "manifest contains an invalid ref name or object id",
                ));
            }
        }
    }
    if (RefState { refs, head }).hash() != manifest.ref_state_hash {
        return Err(corrupt("manifest refs do not match ref_state_hash"));
    }
    Ok(())
}

pub fn valid_snapshot_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
pub fn valid_ref_name(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.contains("..")
        && !name.contains(['\\', ':'])
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
}
pub fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}
pub fn checksum_digest(checksum: &str) -> Result<&str> {
    checksum
        .strip_prefix("sha256:")
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or_else(|| corrupt("artifact checksum is not a lowercase SHA-256 digest"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid_manifest() -> Manifest {
        let state = RefState {
            refs: BTreeMap::from([(
                "refs/heads/main".into(),
                "0123456789abcdef0123456789abcdef01234567".into(),
            )]),
            head: Some("refs/heads/main".into()),
        };
        let snapshot_id = "20260919T103000Z-g42-a1b2c3d4".to_owned();
        Manifest {
            schema_version: 2,
            repo_id: Uuid::nil(),
            repo_name: "notes".into(),
            instance_id: Uuid::nil(),
            snapshot_id: snapshot_id.clone(),
            created_at: "2026-09-19T10:30:00Z".into(),
            generation: 42,
            refuge_version: "1.0.0".into(),
            ref_state_hash: state.hash(),
            refs: Manifest::refs_from(&state),
            git: GitSection {
                parent: None,
                bundle: Some(Artifact {
                    key: crate::layout::RepoLayout::bundle_key(&snapshot_id),
                    size: 42,
                    checksum: format!("sha256:{}", "a".repeat(64)),
                }),
            },
            lfs: None,
        }
    }
    fn path() -> std::path::PathBuf {
        "20260919T103000Z-g42-a1b2c3d4.json".into()
    }
    #[test]
    fn round_trips_and_validates_v2() {
        let manifest = valid_manifest();
        validate(&path(), &manifest).unwrap();
        assert_eq!(
            serde_json::from_slice::<Manifest>(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
            manifest
        );
    }
    #[test]
    fn rejects_structural_invariants() {
        let mut values = Vec::new();
        let mut value = valid_manifest();
        value.schema_version = 1;
        values.push(value);
        let mut value = valid_manifest();
        value.generation = 0;
        values.push(value);
        let mut value = valid_manifest();
        value.git.parent = Some(value.snapshot_id.clone());
        values.push(value);
        let mut value = valid_manifest();
        value.git.bundle = None;
        values.push(value);
        for value in values {
            assert!(validate(&path(), &value).is_err());
        }
        assert!(validate(Path::new("wrong.json"), &valid_manifest()).is_err());
    }
}
