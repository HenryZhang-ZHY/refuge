use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{git, storage};

const MAX_POINTER_BYTES: u64 = 1024;
const MAX_ARCHIVE_ENTRIES: usize = 1_000_000;
const MAX_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub oid: String,
    pub size: u64,
}

pub fn requirements(repo: &Path) -> Result<Vec<Requirement>> {
    let mut requirements = BTreeMap::new();
    for oid in git::reachable_objects(repo)? {
        if git::object_type(repo, &oid)? != "blob" {
            continue;
        }
        let size = git::object_size(repo, &oid)?;
        if size > MAX_POINTER_BYTES {
            continue;
        }
        let contents = git::object_contents(repo, &oid)?;
        if let Some(requirement) = parse_pointer(&contents)?
            && let Some(previous) = requirements.insert(requirement.oid.clone(), requirement.size)
            && previous != requirement.size
        {
            bail!(
                "LFS pointer history gives conflicting sizes for {}",
                requirement.oid
            );
        }
    }
    Ok(requirements
        .into_iter()
        .map(|(oid, size)| Requirement { oid, size })
        .collect())
}

fn parse_pointer(contents: &[u8]) -> Result<Option<Requirement>> {
    let Ok(text) = std::str::from_utf8(contents) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    if lines.next() != Some("version https://git-lfs.github.com/spec/v1") {
        return Ok(None);
    }
    let oid = lines
        .find_map(|line| line.strip_prefix("oid sha256:"))
        .context("Git LFS pointer has no SHA-256 oid")?;
    if oid.len() != 64 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git LFS pointer has an invalid SHA-256 oid");
    }
    let size = text
        .lines()
        .find_map(|line| line.strip_prefix("size "))
        .context("Git LFS pointer has no size")?
        .parse::<u64>()
        .context("Git LFS pointer has an invalid size")?;
    Ok(Some(Requirement {
        oid: oid.to_ascii_lowercase(),
        size,
    }))
}

pub fn create_archive(repo: &Path, destination: &Path) -> Result<bool> {
    let required = requirements(repo)?;
    let objects = repo.join("lfs").join("objects");
    validate_requirements(&objects, &required)?;
    let files = present_objects(&objects)?;
    if files.is_empty() {
        return Ok(false);
    }
    let file = File::create(destination)
        .with_context(|| format!("could not create {}", destination.display()))?;
    let mut archive = tar::Builder::new(file);
    for (path, relative) in files {
        archive
            .append_path_with_name(&path, &relative)
            .with_context(|| format!("could not archive {}", path.display()))?;
    }
    archive
        .into_inner()
        .with_context(|| format!("could not finish archive {}", destination.display()))?;
    Ok(true)
}

pub fn extract_archive(archive: &Path, repo: &Path) -> Result<()> {
    let destination = repo.join("lfs").join("objects");
    fs::create_dir_all(&destination)
        .with_context(|| format!("could not create {}", destination.display()))?;
    let file =
        File::open(archive).with_context(|| format!("could not open {}", archive.display()))?;
    let mut archive = tar::Archive::new(file);
    let mut entries = 0_usize;
    let mut expanded = 0_u64;
    for item in archive
        .entries()
        .context("could not read LFS archive entries")?
    {
        let mut entry = item.context("could not read LFS archive entry")?;
        entries = entries
            .checked_add(1)
            .context("LFS archive entry count overflow")?;
        if entries > MAX_ARCHIVE_ENTRIES {
            bail!("LFS archive exceeds the entry-count limit");
        }
        expanded = expanded
            .checked_add(entry.size())
            .context("LFS archive expanded size overflow")?;
        if expanded > MAX_EXPANDED_BYTES {
            bail!("LFS archive exceeds the expanded-size limit");
        }
        let relative = normalized_entry_path(&entry.path()?)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let output = destination.join(&relative);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if !entry.header().entry_type().is_file() || !valid_object_relative_path(&relative) {
            bail!("LFS archive contains a nonregular or unexpected entry");
        }
        let parent = output.parent().context("LFS archive entry has no parent")?;
        fs::create_dir_all(parent)?;
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .with_context(|| format!("could not create extracted object {}", output.display()))?;
        io::copy(&mut entry, &mut target)
            .with_context(|| format!("could not extract object {}", output.display()))?;
    }
    verify_repository(repo)
}

pub fn verify_repository(repo: &Path) -> Result<()> {
    let required = requirements(repo)?;
    validate_requirements(&repo.join("lfs").join("objects"), &required)
}

fn validate_requirements(objects: &Path, requirements: &[Requirement]) -> Result<()> {
    for requirement in requirements {
        let path = object_path(objects, &requirement.oid);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("required LFS object {} is missing", requirement.oid))?;
        if !metadata.file_type().is_file() {
            bail!(
                "required LFS object {} is not a regular file",
                requirement.oid
            );
        }
        if metadata.len() != requirement.size {
            bail!("required LFS object {} has the wrong size", requirement.oid);
        }
        verify_object(&path, &requirement.oid)?;
    }
    Ok(())
}

fn present_objects(objects: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    if !objects.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut stack = vec![objects.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("could not read directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let relative = path.strip_prefix(objects)?.to_path_buf();
                if !valid_object_relative_path(&relative) {
                    bail!("unexpected file in LFS object store: {}", path.display());
                }
                let oid = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("LFS object path is not valid UTF-8")?;
                verify_object(&path, oid)?;
                files.push((path, relative));
            } else {
                bail!("nonregular entry in LFS object store: {}", path.display());
            }
        }
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(files)
}

fn verify_object(path: &Path, oid: &str) -> Result<()> {
    let (actual, _) = storage::checksum(path)?;
    if actual != format!("sha256:{oid}") {
        bail!("LFS object {} is corrupt", path.display());
    }
    Ok(())
}

fn object_path(objects: &Path, oid: &str) -> PathBuf {
    objects.join(&oid[..2]).join(&oid[2..4]).join(oid)
}

fn valid_object_relative_path(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    let [
        Component::Normal(first),
        Component::Normal(second),
        Component::Normal(file),
    ] = components.as_slice()
    else {
        return false;
    };
    let (Some(first), Some(second), Some(file)) = (first.to_str(), second.to_str(), file.to_str())
    else {
        return false;
    };
    file.len() == 64
        && file.bytes().all(|byte| byte.is_ascii_hexdigit())
        && first == &file[..2]
        && second == &file[2..4]
}

fn normalized_entry_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            _ => bail!("LFS archive entry escapes the object directory"),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_pointer() {
        let oid = "a".repeat(64);
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 42\n");
        assert_eq!(
            parse_pointer(pointer.as_bytes()).unwrap(),
            Some(Requirement { oid, size: 42 })
        );
    }

    #[test]
    fn rejects_nonregular_archive_entries() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("bad.tar");
        let file = File::create(&archive_path).unwrap();
        let mut archive = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        archive
            .append_data(&mut header, "aa/bb/link", io::empty())
            .unwrap();
        archive.finish().unwrap();
        let repo = temp.path().join("repo.git");
        fs::create_dir(&repo).unwrap();
        assert!(
            extract_archive(&archive_path, &repo)
                .unwrap_err()
                .to_string()
                .contains("nonregular")
        );
    }
}
