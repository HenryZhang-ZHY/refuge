use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed,
    CommittedWithWarnings(Vec<String>),
}

impl CommitOutcome {
    pub fn warnings(&self) -> &[String] {
        match self {
            Self::Committed => &[],
            Self::CommittedWithWarnings(warnings) => warnings,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FsSnapshotStore;

impl FsSnapshotStore {
    pub fn publish_file(
        &self,
        source: &Path,
        destination: &Path,
        expected_checksum: &str,
        expected_size: u64,
    ) -> Result<CommitOutcome> {
        let parent = destination
            .parent()
            .context("snapshot destination has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
        let mut partial = tempfile::Builder::new()
            .prefix(".refuge-partial-")
            .tempfile_in(parent)
            .with_context(|| format!("could not create partial file in {}", parent.display()))?;
        let mut input = File::open(source)
            .with_context(|| format!("could not open staged artifact {}", source.display()))?;
        std::io::copy(&mut input, partial.as_file_mut()).with_context(|| {
            format!(
                "could not copy staged artifact {} into {}",
                source.display(),
                parent.display()
            )
        })?;
        self.finish_publish(
            partial,
            destination,
            expected_checksum,
            expected_size,
            Some(source),
        )
    }

    pub fn publish_bytes(&self, bytes: &[u8], destination: &Path) -> Result<CommitOutcome> {
        let parent = destination
            .parent()
            .context("snapshot destination has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
        let mut partial = tempfile::Builder::new()
            .prefix(".refuge-partial-")
            .tempfile_in(parent)
            .with_context(|| format!("could not create partial file in {}", parent.display()))?;
        partial
            .write_all(bytes)
            .with_context(|| format!("could not write partial file in {}", parent.display()))?;
        let checksum = checksum_bytes(bytes);
        self.finish_publish(partial, destination, &checksum, bytes.len() as u64, None)
    }

    fn finish_publish(
        &self,
        partial: tempfile::NamedTempFile,
        destination: &Path,
        expected_checksum: &str,
        expected_size: u64,
        staged_source: Option<&Path>,
    ) -> Result<CommitOutcome> {
        partial.as_file().sync_all().with_context(|| {
            format!("could not flush partial file for {}", destination.display())
        })?;
        let (actual_checksum, actual_size) = checksum(partial.path())?;
        if actual_size != expected_size || actual_checksum != expected_checksum {
            bail!(
                "partial file for {} differs from the staged content",
                destination.display()
            );
        }
        partial.persist_noclobber(destination).map_err(|error| {
            anyhow::anyhow!(error.error).context(format!(
                "could not publish {} without overwriting an existing snapshot",
                destination.display()
            ))
        })?;

        let mut warnings = Vec::new();
        if let Some(parent) = destination.parent()
            && let Err(error) = sync_directory(parent)
        {
            warnings.push(format!(
                "snapshot committed, but directory durability could not be confirmed for {}: {error}",
                parent.display()
            ));
        }
        if let Some(source) = staged_source
            && let Err(error) = fs::remove_file(source)
        {
            warnings.push(format!(
                "snapshot committed, but staged source {} could not be removed: {error}",
                source.display()
            ));
        }
        if warnings.is_empty() {
            Ok(CommitOutcome::Committed)
        } else {
            Ok(CommitOutcome::CommittedWithWarnings(warnings))
        }
    }
}

pub fn checksum(path: &Path) -> Result<(String, u64)> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;
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

fn checksum_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory flush is not supported by this platform policy",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_written_bytes_and_never_overwrites_a_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("snapshot.bundle");
        fs::write(&source, b"new snapshot").unwrap();
        fs::write(&destination, b"existing snapshot").unwrap();
        let (checksum, size) = checksum(&source).unwrap();

        let error = FsSnapshotStore
            .publish_file(&source, &destination, &checksum, size)
            .unwrap_err();

        assert!(error.to_string().contains("without overwriting"));
        assert_eq!(fs::read(&destination).unwrap(), b"existing snapshot");
        assert_eq!(fs::read(&source).unwrap(), b"new snapshot");
    }

    #[test]
    fn rejects_a_copy_that_does_not_match_the_prepared_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("snapshot.bundle");
        fs::write(&source, b"changed content").unwrap();

        let error = FsSnapshotStore
            .publish_file(
                &source,
                &destination,
                &format!("sha256:{}", "0".repeat(64)),
                15,
            )
            .unwrap_err();

        assert!(error.to_string().contains("differs"));
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).unwrap(), b"changed content");
    }

    #[test]
    fn publishes_verified_content_and_removes_its_staged_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("snapshot.bundle");
        fs::write(&source, b"prepared snapshot").unwrap();
        let (checksum, size) = checksum(&source).unwrap();

        let outcome = FsSnapshotStore
            .publish_file(&source, &destination, &checksum, size)
            .unwrap();

        assert!(outcome.warnings().is_empty());
        assert_eq!(fs::read(&destination).unwrap(), b"prepared snapshot");
        assert!(!source.exists());
    }
}
