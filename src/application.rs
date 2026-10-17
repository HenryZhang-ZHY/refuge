use std::fmt;
use std::path::Path;

use anyhow::{Result, bail};

use crate::backup::{self, BackupOutcome};
use crate::catalog::{self, ProtectionState};
use crate::config::Config;
use crate::repo::{self, HostedRepository, Repository};

pub struct ProvisionedRepository {
    pub repository: Repository,
    pub backup: BackupOutcome,
}

#[derive(Debug)]
pub enum ProvisionError {
    Setup(anyhow::Error),
    InitialBackup {
        repository: Repository,
        source: anyhow::Error,
    },
}

impl ProvisionError {
    pub fn repository(&self) -> Option<&Repository> {
        match self {
            Self::Setup(_) => None,
            Self::InitialBackup { repository, .. } => Some(repository),
        }
    }
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "{error:#}"),
            Self::InitialBackup { repository, source } => write!(
                formatter,
                "repository was created at {}, but its initial backup failed: {source:#}; retry with `refuge repo backup {}`",
                repository.path.display(),
                repository
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<NAME>")
            ),
        }
    }
}

impl std::error::Error for ProvisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup(error) => Some(error.as_ref()),
            Self::InitialBackup { source, .. } => Some(source.as_ref()),
        }
    }
}

pub fn create_repository(
    config: &Config,
    name: &str,
) -> Result<ProvisionedRepository, ProvisionError> {
    provision(
        repo::create(config, name).map_err(ProvisionError::Setup)?,
        config,
    )
}

pub fn import_repository(
    config: &Config,
    name: &str,
    source: &Path,
) -> Result<ProvisionedRepository, ProvisionError> {
    provision(
        repo::import(config, name, source).map_err(ProvisionError::Setup)?,
        config,
    )
}

fn provision(
    repository: Repository,
    config: &Config,
) -> Result<ProvisionedRepository, ProvisionError> {
    match backup::backup_path(config, &repository.path, backup::BackupOptions::default()) {
        Ok(backup) => Ok(ProvisionedRepository { repository, backup }),
        Err(source) => Err(ProvisionError::InitialBackup { repository, source }),
    }
}

/// Removes a live repository while preserving every snapshot in the target.
/// Unless forced, refuses when the newest snapshot does not hold the current
/// refs, because removal would then lose history.
pub fn remove_repository(config: &Config, selector: &str, force: bool) -> Result<HostedRepository> {
    let repository = repo::resolve(config, selector)?;
    let _name_lock = repo::lock_name(config, &repository.name)?;
    let _identity_lock = repo::lock_identity(config, repository.id)?;
    let _backup_lock = backup::lock_repository(config, repository.id)?;
    if !force {
        let reason = match catalog::repository_status(config, &repository)? {
            ProtectionState::Protected { .. } => None,
            ProtectionState::Pending => {
                Some("has changes that are not in its newest snapshot".to_owned())
            }
            ProtectionState::Unprotected => Some("has no snapshot".to_owned()),
            ProtectionState::Corrupt { reason } => {
                Some(format!("has no valid newest snapshot ({reason})"))
            }
        };
        if let Some(reason) = reason {
            bail!(
                "repository {} {reason}; run `refuge repo backup {}` first, or pass --force to remove it anyway",
                repository.name,
                repository.name
            );
        }
    }
    repo::delete(config, &repository)?;
    Ok(repository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn initial_backup_failure_preserves_the_configured_repository() {
        let temp = tempfile::tempdir().unwrap();
        let blocked_target = temp.path().join("target-is-a-file");
        std::fs::write(&blocked_target, b"not a directory").unwrap();
        let config = Config {
            repos_dir: temp.path().join("repos"),
            target_root: blocked_target,
            instance_id: Uuid::nil(),
        };

        let error = match create_repository(&config, "notes") {
            Ok(_) => panic!("backup unexpectedly succeeded"),
            Err(error) => error,
        };

        let repository = error.repository().expect("partial success repository");
        assert!(repository.path.is_dir());
        assert!(repository.path.join("hooks/post-receive").is_file());
        assert!(error.to_string().contains("refuge repo backup notes"));
    }
}
