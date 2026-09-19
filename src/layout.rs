use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use time::{OffsetDateTime, macros::format_description};
use uuid::Uuid;

use crate::config::Config;

pub const TARGET_ROOT: &str = "refuge/v2/repos";

#[derive(Debug, Clone)]
pub struct RepoLayout {
    root: PathBuf,
}

impl RepoLayout {
    pub fn new(target: &Path, repo_id: Uuid) -> Self {
        Self {
            root: target.join(TARGET_ROOT).join(repo_id.to_string()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn manifest_path(&self, snapshot_id: &str) -> PathBuf {
        self.snapshots_dir().join(format!("{snapshot_id}.json"))
    }

    pub fn bundle_key(snapshot_id: &str) -> String {
        format!("git/{snapshot_id}.bundle")
    }

    pub fn lfs_object_key(oid: &str) -> String {
        format!("lfs/objects/{}/{oid}", &oid[..2])
    }

    pub fn lfs_set_key(hex: &str) -> String {
        format!("lfs/sets/{hex}.txt")
    }

    pub fn resolve(&self, key: &str) -> Result<PathBuf> {
        let path = Path::new(key);
        if path.is_absolute()
            || key.contains('\\')
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!("invalid store key {key}");
        }
        let parts = path
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>();
        let valid = match parts.as_slice() {
            [git, name] if git == "git" => name.ends_with(".bundle") && name.len() > 7,
            [snapshots, name] if snapshots == "snapshots" => {
                name.ends_with(".json") && name.len() > 5
            }
            [lfs, sets, name] if lfs == "lfs" && sets == "sets" => {
                name.strip_suffix(".txt").is_some_and(is_sha256_hex)
            }
            [lfs, objects, shard, oid] if lfs == "lfs" && objects == "objects" => {
                shard.len() == 2 && is_sha256_hex(oid) && shard.as_ref() == &oid[..2]
            }
            _ => false,
        };
        if !valid {
            bail!("invalid store key {key}");
        }
        Ok(self.root.join(path))
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub fn locks_dir(config: &Config) -> PathBuf {
    config.repos_dir.join(".refuge-locks")
}

pub fn staging_dir(config: &Config) -> PathBuf {
    config.repos_dir.join(".refuge-staging")
}

pub fn snapshot_id(now: OffsetDateTime, generation: u64, instance: Uuid) -> String {
    let compact = now
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .expect("fixed UTC timestamp format");
    let instance = instance.simple().to_string();
    format!("{compact}-g{generation}-{}", &instance[..8])
}

pub fn generation_from_file_name(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".json")?;
    let (_, suffix) = stem.rsplit_once("-g")?;
    let (generation, _) = suffix.split_once('-')?;
    if generation.is_empty() || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    generation.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_v2_store_key_shapes() {
        let layout = RepoLayout::new(Path::new("/target"), Uuid::nil());
        let oid = "a".repeat(64);
        for key in [
            "git/id.bundle".to_owned(),
            "snapshots/id.json".to_owned(),
            format!("lfs/sets/{oid}.txt"),
            format!("lfs/objects/aa/{oid}"),
        ] {
            assert!(layout.resolve(&key).is_ok(), "{key}");
        }
        for key in [
            "../git/id.bundle",
            "/git/id.bundle",
            "git\\id.bundle",
            "git/id",
            "lfs/objects/ab/not-an-oid",
            "other/id.json",
        ] {
            assert!(layout.resolve(key).is_err(), "{key}");
        }
    }

    #[test]
    fn parses_generation_from_any_snapshot_filename() {
        assert_eq!(
            generation_from_file_name("20260919T103000Z-g42-a1b2c3d4.json"),
            Some(42)
        );
        assert_eq!(generation_from_file_name("conflict-g9-copy.json"), Some(9));
        assert_eq!(generation_from_file_name("snapshot.json"), None);
    }
}
