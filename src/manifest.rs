use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::git::RefState;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

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
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ManifestValidationError {}

fn unsupported(message: impl Into<String>) -> anyhow::Error {
    ManifestValidationError {
        issue: ManifestIssue::Unsupported,
        message: message.into(),
    }
    .into()
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
    pub format: String,
    pub format_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub repo_id: Uuid,
    pub repo_name: String,
    pub instance_id: Uuid,
    pub snapshot_id: String,
    pub created_at: String,
    pub generation: u64,
    pub ref_state_hash: String,
    pub refs: BTreeMap<String, ManifestRef>,
    pub artifact: Option<Artifact>,
    /// Archive of the hosted repository's `lfs/objects` content store, if
    /// the repository has any Git LFS objects. `git bundle` only captures
    /// Git objects, never the LFS content store, so it is snapshotted and
    /// verified separately.
    pub lfs_artifact: Option<Artifact>,
    pub encryption: Option<Value>,
    pub refuge_version: String,
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
                "HEAD".to_owned(),
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
}

pub fn paths_in(snapshots: &Path) -> Result<Vec<PathBuf>> {
    if !snapshots.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(snapshots)? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".manifest.json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn read(path: &Path) -> Result<Manifest> {
    let file =
        File::open(path).with_context(|| format!("could not open manifest {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read manifest {}", path.display()))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!(
            "manifest {} exceeds the {} byte limit",
            path.display(),
            MAX_MANIFEST_BYTES
        );
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid manifest {}", path.display()))?;
    validate(path, &manifest)?;
    Ok(manifest)
}

pub fn validate(path: &Path, manifest: &Manifest) -> Result<()> {
    if manifest.schema_version != 1 {
        return Err(unsupported(format!(
            "unsupported manifest schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.encryption.is_some() {
        return Err(unsupported("unsupported manifest encryption"));
    }
    if manifest.generation == 0 {
        bail!("manifest generation must be at least one");
    }
    if manifest.snapshot_id.is_empty()
        || !manifest
            .snapshot_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("manifest snapshot id contains invalid path characters");
    }
    let expected_file = format!("{}.manifest.json", manifest.snapshot_id);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_file.as_str()) {
        bail!("manifest snapshot id differs from its filename");
    }

    validate_refs(manifest)?;
    match &manifest.artifact {
        Some(artifact) => validate_artifact(
            artifact,
            &format!("snapshots/{}.bundle", manifest.snapshot_id),
            "git-bundle",
            2,
        )?,
        None if manifest
            .refs
            .iter()
            .any(|(name, value)| name != "HEAD" || matches!(value, ManifestRef::Object(_))) =>
        {
            bail!("non-empty snapshot has no Git bundle artifact");
        }
        None => {}
    }
    if let Some(artifact) = &manifest.lfs_artifact {
        validate_artifact(
            artifact,
            &format!("snapshots/{}.lfs.tar", manifest.snapshot_id),
            "lfs-archive",
            1,
        )?;
    }
    Ok(())
}

fn validate_refs(manifest: &Manifest) -> Result<()> {
    let mut refs = BTreeMap::new();
    let mut head = None;
    for (name, value) in &manifest.refs {
        match (name.as_str(), value) {
            ("HEAD", ManifestRef::Symbolic { symref }) if valid_ref_name(symref) => {
                head = Some(symref.clone());
            }
            ("HEAD", _) => bail!("manifest HEAD must be a valid symbolic ref"),
            (_, ManifestRef::Object(oid)) if valid_ref_name(name) && valid_oid(oid) => {
                refs.insert(name.clone(), oid.clone());
            }
            (_, ManifestRef::Symbolic { .. }) => {
                bail!("only manifest HEAD may be symbolic")
            }
            _ => bail!("manifest contains an invalid ref name or object id"),
        }
    }
    let actual = RefState { refs, head }.hash();
    if actual != manifest.ref_state_hash {
        bail!("manifest refs do not match ref_state_hash");
    }
    Ok(())
}

fn validate_artifact(
    artifact: &Artifact,
    expected_key: &str,
    expected_format: &str,
    expected_version: u32,
) -> Result<()> {
    if artifact.key != expected_key {
        bail!("artifact key does not match the snapshot layout");
    }
    if artifact.format != expected_format || artifact.format_version != expected_version {
        return Err(unsupported(format!(
            "unsupported artifact format {} version {}",
            artifact.format, artifact.format_version
        )));
    }
    if !valid_checksum(&artifact.checksum) {
        bail!("artifact checksum is not a SHA-256 digest");
    }
    Ok(())
}

fn valid_ref_name(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.contains("..")
        && !name.contains(['\\', ':'])
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_checksum(checksum: &str) -> bool {
    checksum.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> Manifest {
        let state = RefState {
            refs: BTreeMap::from([(
                "refs/heads/main".to_owned(),
                "0123456789abcdef0123456789abcdef01234567".to_owned(),
            )]),
            head: Some("refs/heads/main".to_owned()),
        };
        Manifest {
            schema_version: 1,
            repo_id: Uuid::nil(),
            repo_name: "notes".to_owned(),
            instance_id: Uuid::nil(),
            snapshot_id: "20260918T091530Z-g1-00000000".to_owned(),
            created_at: "2026-09-18T09:15:30Z".to_owned(),
            generation: 1,
            ref_state_hash: state.hash(),
            refs: Manifest::refs_from(&state),
            artifact: Some(Artifact {
                key: "snapshots/20260918T091530Z-g1-00000000.bundle".to_owned(),
                size: 42,
                checksum: format!("sha256:{}", "a".repeat(64)),
                format: "git-bundle".to_owned(),
                format_version: 2,
            }),
            lfs_artifact: None,
            encryption: None,
            refuge_version: "0.1.0".to_owned(),
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("20260918T091530Z-g1-00000000.manifest.json")
    }

    #[test]
    fn validates_manifest_semantics() {
        validate(&path(), &valid_manifest()).unwrap();
    }

    #[test]
    fn rejects_unsupported_schema_and_encryption() {
        let mut manifest = valid_manifest();
        manifest.schema_version = 2;
        assert!(
            validate(&path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("schema")
        );

        let mut manifest = valid_manifest();
        manifest.encryption = Some(serde_json::json!({"scheme": "future"}));
        assert!(
            validate(&path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("encryption")
        );
    }

    #[test]
    fn rejects_mismatched_filename_refs_hash_and_artifact_layout() {
        let manifest = valid_manifest();
        assert!(validate(Path::new("other.manifest.json"), &manifest).is_err());

        let mut manifest = valid_manifest();
        manifest.ref_state_hash = format!("sha256:{}", "0".repeat(64));
        assert!(
            validate(&path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("ref_state_hash")
        );

        for key in [
            "/absolute.bundle",
            "snapshots/../escape.bundle",
            "snapshots\\escape.bundle",
            "C:\\escape.bundle",
        ] {
            let mut manifest = valid_manifest();
            manifest.artifact.as_mut().unwrap().key = key.to_owned();
            assert!(validate(&path(), &manifest).is_err(), "accepted {key}");
        }
    }

    #[test]
    fn old_manifest_without_lfs_artifact_remains_readable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(path());
        let mut value = serde_json::to_value(valid_manifest()).unwrap();
        value.as_object_mut().unwrap().remove("lfs_artifact");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read(&path).unwrap().lfs_artifact.is_none());
    }

    #[test]
    fn bounds_manifest_input_size() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.manifest.json");
        std::fs::write(&path, vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("exceeds"));
    }
}
