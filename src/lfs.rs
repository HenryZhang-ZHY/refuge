use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::git::{self, BatchCheck};
use crate::manifest::Artifact;

const MAX_POINTER_BYTES: u64 = 1024;
pub type LfsSet = BTreeMap<String, u64>;

pub fn required_set(repo: &Path) -> Result<LfsSet> {
    let objects = git::reachable_objects(repo)?;
    let checks = git::batch_check(repo, &objects)?;
    let blobs = objects
        .into_iter()
        .zip(checks)
        .filter_map(|(name, check)| match check {
            BatchCheck::Found { oid, kind, size }
                if kind == "blob" && size <= MAX_POINTER_BYTES =>
            {
                Some((name, oid))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let oids = blobs.iter().map(|(_, oid)| oid.clone()).collect::<Vec<_>>();
    let contents = git::batch_blob_contents(repo, &oids)?;
    let mut set = BTreeMap::new();
    for (_, bytes) in contents {
        if let Some((oid, size)) = parse_pointer(&bytes)?
            && let Some(previous) = set.insert(oid.clone(), size)
            && previous != size
        {
            bail!("LFS pointer history gives conflicting sizes for {oid}");
        }
    }
    Ok(set)
}

fn parse_pointer(contents: &[u8]) -> Result<Option<(String, u64)>> {
    let Ok(text) = std::str::from_utf8(contents) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    if lines.next() != Some("version https://git-lfs.github.com/spec/v1") {
        return Ok(None);
    }
    let oid = text
        .lines()
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
    Ok(Some((oid.to_ascii_lowercase(), size)))
}

pub fn encode_set(set: &LfsSet) -> (Vec<u8>, Artifact) {
    let mut bytes = Vec::new();
    for (oid, size) in set {
        bytes.extend_from_slice(format!("{oid} {size}\n").as_bytes());
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let artifact = Artifact {
        key: crate::layout::RepoLayout::lfs_set_key(&digest),
        size: bytes.len() as u64,
        checksum: format!("sha256:{digest}"),
    };
    (bytes, artifact)
}

pub fn decode_set(bytes: &[u8], expected_checksum: &str) -> Result<LfsSet> {
    let expected = crate::manifest::checksum_digest(expected_checksum)?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        bail!("LFS set checksum differs from its key");
    }
    if bytes.is_empty() {
        bail!("LFS set is empty");
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bail!("LFS set has no final newline");
    }
    let text = std::str::from_utf8(bytes).context("LFS set is not UTF-8")?;
    let mut set = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for line in text.lines() {
        let (oid, size) = line.split_once(' ').context("malformed LFS set line")?;
        if line.matches(' ').count() != 1
            || oid.len() != 64
            || !oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            bail!("malformed LFS set oid");
        }
        if previous.is_some_and(|item| item >= oid) {
            bail!("LFS set is unsorted or contains duplicates");
        }
        let size = size.parse::<u64>().context("malformed LFS set size")?;
        if size.to_string() != line.split_once(' ').unwrap().1 {
            bail!("non-canonical LFS set size");
        }
        set.insert(oid.to_owned(), size);
        previous = Some(oid);
    }
    Ok(set)
}

pub fn local_object_path(repo: &Path, oid: &str) -> PathBuf {
    repo.join("lfs")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn set_encoding_is_canonical_and_strict() {
        let set = BTreeMap::from([("a".repeat(64), 1), ("b".repeat(64), 42)]);
        let (bytes, artifact) = encode_set(&set);
        assert_eq!(decode_set(&bytes, &artifact.checksum).unwrap(), set);
        let bad = format!("{} 01\n", "a".repeat(64));
        let digest = format!("sha256:{:x}", Sha256::digest(bad.as_bytes()));
        assert!(decode_set(bad.as_bytes(), &digest).is_err());
    }
    #[test]
    fn parses_pointer() {
        let oid = "a".repeat(64);
        let bytes =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 42\n");
        assert_eq!(parse_pointer(bytes.as_bytes()).unwrap(), Some((oid, 42)));
    }
}
