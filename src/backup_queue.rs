use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use uuid::Uuid;

use crate::catalog::ProtectionState;
use crate::config::Config;
use crate::{backup, git, repo};

pub fn enqueue(config: &Config, repository: &Path, store_root: &Path) -> Result<()> {
    let id = Uuid::parse_str(&git::config_get(repository, "refuge.repoid")?)
        .context("repository has an invalid refuge.repoid")?;
    enqueue_id(config, id, store_root)
}

pub fn reconcile(config: &Config, store_root: &Path) -> Result<()> {
    for repository in repo::list(config)? {
        if !matches!(
            crate::catalog::repository_status(config, &repository)?,
            ProtectionState::Protected { .. }
        ) {
            enqueue_id(config, repository.id, store_root)?;
        }
    }
    Ok(())
}

pub fn process_once(config: &Config, store_root: &Path) -> Result<()> {
    let queue = queue_dir(store_root);
    std::fs::create_dir_all(&queue)?;
    for entry in std::fs::read_dir(&queue)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("job") {
            continue;
        }
        if let Err(error) = process_job(config, &path) {
            eprintln!("refuge: queued backup remains pending: {error:#}");
        }
    }
    Ok(())
}

fn enqueue_id(_config: &Config, id: Uuid, store_root: &Path) -> Result<()> {
    let queue = queue_dir(store_root);
    std::fs::create_dir_all(&queue)
        .with_context(|| format!("could not create backup queue {}", queue.display()))?;
    let _lock = lock_job(&queue, id)?;
    let marker = queue.join(format!("{id}.job"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&marker)
        .with_context(|| format!("could not write backup job {}", marker.display()))?;
    writeln!(file, "{}", Uuid::now_v7())?;
    file.sync_all()?;
    sync_directory(&queue)?;
    Ok(())
}

fn process_job(config: &Config, marker: &Path) -> Result<()> {
    let id = marker
        .file_stem()
        .and_then(|value| value.to_str())
        .context("backup job has an invalid filename")?;
    let id = Uuid::parse_str(id).context("backup job has an invalid repository id")?;
    let queue = marker.parent().context("backup job has no parent")?;
    let token = {
        let _lock = lock_job(queue, id)?;
        std::fs::read(marker).with_context(|| format!("could not read {}", marker.display()))?
    };
    let repository = repo::resolve(config, &id.to_string())?;
    backup::backup_path(config, &repository.path, backup::BackupOptions::default())?;

    let _lock = lock_job(queue, id)?;
    if std::fs::read(marker).ok().as_deref() == Some(token.as_slice()) {
        std::fs::remove_file(marker)?;
        sync_directory(queue)?;
    }
    Ok(())
}

fn queue_dir(store_root: &Path) -> PathBuf {
    store_root.join("queue")
}

fn lock_job(queue: &Path, id: Uuid) -> Result<std::fs::File> {
    let path = queue.join(format!("{id}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    file.lock_exclusive()?;
    Ok(file)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
