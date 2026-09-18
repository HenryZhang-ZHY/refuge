use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::git::RefState;

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
