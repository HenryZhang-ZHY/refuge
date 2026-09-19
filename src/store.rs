use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::layout::RepoLayout;
use crate::manifest::Artifact;

#[derive(Debug, Default)]
pub struct Written {
    pub bytes_written: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum CopyError {
    Missing(String),
    Mismatch(String),
    Io(anyhow::Error),
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(reason) | Self::Mismatch(reason) => formatter.write_str(reason),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CopyError {}

#[derive(Debug, Clone)]
pub struct Store {
    layout: RepoLayout,
}

impl Store {
    pub fn new(layout: RepoLayout) -> Self {
        Self { layout }
    }

    pub fn layout(&self) -> &RepoLayout {
        &self.layout
    }

    pub fn stat(&self, key: &str) -> Result<Option<u64>> {
        let path = self.layout.resolve(key)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(Some(metadata.len())),
            Ok(_) => bail!("store path {} is not a regular file", path.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("could not inspect {}", path.display()))
            }
        }
    }

    pub fn read(&self, key: &str, max: u64) -> Result<Vec<u8>> {
        let path = self.layout.resolve(key)?;
        let file =
            File::open(&path).with_context(|| format!("could not open {}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(max + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("could not read {}", path.display()))?;
        if bytes.len() as u64 > max {
            bail!("store file {} exceeds the {max} byte limit", path.display());
        }
        Ok(bytes)
    }

    pub fn publish_file(&self, source: &Path, key: &str, expected: &Artifact) -> Result<Written> {
        if expected.key != key {
            bail!(
                "artifact key {} does not match destination {key}",
                expected.key
            );
        }
        let mut input = File::open(source)
            .with_context(|| format!("could not open source {}", source.display()))?;
        self.publish_reader(&mut input, key, Some(expected))
    }

    pub fn publish_bytes(&self, bytes: &[u8], key: &str) -> Result<Written> {
        self.publish_reader(&mut io::Cursor::new(bytes), key, None)
    }

    fn publish_reader(
        &self,
        input: &mut dyn Read,
        key: &str,
        expected: Option<&Artifact>,
    ) -> Result<Written> {
        let final_path = self.layout.resolve(key)?;
        let parent = final_path.parent().context("store path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
        let mut partial = tempfile::Builder::new()
            .prefix(".refuge-")
            .suffix(".tmp")
            .tempfile_in(parent)
            .with_context(|| format!("could not stage in {}", parent.display()))?;
        let (checksum, size) = copy_and_hash(input, partial.as_file_mut())?;
        if let Some(expected) = expected {
            verify_descriptor(&checksum, size, expected)?;
        }
        partial.as_file_mut().sync_all()?;
        let (staged_checksum, staged_size) = sha256_file(partial.path())?;
        if staged_checksum != checksum || staged_size != size {
            bail!("staged file changed before publication");
        }
        partial.persist_noclobber(&final_path).map_err(|error| {
            anyhow::anyhow!(error.error).context(format!(
                "would overwrite immutable store file {}",
                final_path.display()
            ))
        })?;
        let mut warnings = Vec::new();
        sync_parent(parent, &mut warnings);
        Ok(Written {
            bytes_written: size,
            warnings,
        })
    }

    pub fn copy_out(
        &self,
        key: &str,
        destination: &Path,
        expected: &Artifact,
    ) -> std::result::Result<(), CopyError> {
        let source = self.layout.resolve(key).map_err(CopyError::Io)?;
        let mut input = match File::open(&source) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CopyError::Missing(format!("{key} is missing")));
            }
            Err(error) => {
                return Err(CopyError::Io(
                    anyhow::Error::new(error)
                        .context(format!("could not open {}", source.display())),
                ));
            }
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| CopyError::Io(error.into()))?;
        }
        let mut output = File::create(destination).map_err(|error| CopyError::Io(error.into()))?;
        let (checksum, size) = copy_and_hash(&mut input, &mut output).map_err(CopyError::Io)?;
        output
            .sync_all()
            .map_err(|error| CopyError::Io(error.into()))?;
        if size != expected.size {
            let _ = fs::remove_file(destination);
            let kind = if size < expected.size {
                CopyError::Missing
            } else {
                CopyError::Mismatch
            };
            return Err(kind(format!(
                "{key} has size {size}, expected {}",
                expected.size
            )));
        }
        if checksum != expected.checksum {
            let _ = fs::remove_file(destination);
            return Err(CopyError::Mismatch(format!(
                "{key} checksum differs from manifest"
            )));
        }
        Ok(())
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        let path = self.layout.resolve(key)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("could not remove {}", path.display()))
            }
        }
    }

    pub fn sweep_partials(&self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        sweep_directory(self.layout.root(), SystemTime::now(), &mut warnings)?;
        Ok(warnings)
    }
}

fn verify_descriptor(checksum: &str, size: u64, expected: &Artifact) -> Result<()> {
    if size != expected.size || checksum != expected.checksum {
        bail!(
            "source content does not match artifact: got {size} bytes {checksum}, expected {} bytes {}",
            expected.size,
            expected.checksum
        );
    }
    Ok(())
}

fn copy_and_hash(input: &mut dyn Read, output: &mut dyn Write) -> Result<(String, u64)> {
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .context("file size overflow")?;
    }
    Ok((format!("sha256:{:x}", digest.finalize()), size))
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut input = File::open(path)?;
    copy_and_hash(&mut input, &mut io::sink())
}

fn sweep_directory(path: &Path, now: SystemTime, warnings: &mut Vec<String>) -> Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_dir() {
            sweep_directory(&entry.path(), now, warnings)?;
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if metadata.file_type().is_file()
            && name.starts_with(".refuge-")
            && name.ends_with(".tmp")
            && metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= Duration::from_secs(24 * 60 * 60))
            && let Err(error) = fs::remove_file(entry.path())
        {
            warnings.push(format!(
                "could not remove stale partial {}: {error}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path, warnings: &mut Vec<String>) {
    if let Err(error) = File::open(path).and_then(|directory| directory.sync_all()) {
        warnings.push(format!(
            "could not sync target directory {}: {error}",
            path.display()
        ));
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path, _warnings: &mut Vec<String>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::RepoLayout;
    use uuid::Uuid;

    fn artifact(key: &str, bytes: &[u8]) -> Artifact {
        let digest = Sha256::digest(bytes);
        Artifact {
            key: key.to_owned(),
            size: bytes.len() as u64,
            checksum: format!("sha256:{digest:x}"),
        }
    }

    #[test]
    fn publish_verifies_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"complete").unwrap();
        let store = Store::new(RepoLayout::new(temp.path(), Uuid::nil()));
        let expected = artifact("git/test.bundle", b"complete");
        assert_eq!(
            store
                .publish_file(&source, &expected.key, &expected)
                .unwrap()
                .bytes_written,
            8
        );
        assert!(
            store
                .publish_file(&source, &expected.key, &expected)
                .is_err()
        );
        assert!(
            store
                .publish_file(
                    &source,
                    "git/wrong.bundle",
                    &artifact("git/wrong.bundle", b"bad")
                )
                .is_err()
        );
    }

    #[test]
    fn copy_out_classifies_missing_and_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(RepoLayout::new(temp.path(), Uuid::nil()));
        let expected = artifact("git/test.bundle", b"complete");
        assert!(matches!(
            store.copy_out(&expected.key, &temp.path().join("out"), &expected),
            Err(CopyError::Missing(_))
        ));
        store.publish_bytes(b"tampered", &expected.key).unwrap();
        assert!(matches!(
            store.copy_out(&expected.key, &temp.path().join("out"), &expected),
            Err(CopyError::Mismatch(_))
        ));
    }
}
