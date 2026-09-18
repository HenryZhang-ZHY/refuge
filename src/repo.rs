use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::{config::Config, git};

#[derive(Debug)]
pub struct Repository {
    pub path: PathBuf,
    pub id: Uuid,
}

#[derive(Debug, Clone)]
pub struct HostedRepository {
    pub name: String,
    pub path: PathBuf,
    pub id: Uuid,
}

pub fn create(config: &Config, name: &str) -> Result<Repository> {
    create_staged(config, name, git::init_bare)
}

pub fn import(config: &Config, name: &str, source: &Path) -> Result<Repository> {
    create_staged(config, name, |path| git::clone_mirror(source, path))
}

fn create_staged(
    config: &Config,
    name: &str,
    initialize: impl FnOnce(&Path) -> Result<()>,
) -> Result<Repository> {
    validate_name(name)?;
    std::fs::create_dir_all(&config.repos_dir).with_context(|| {
        format!(
            "could not create repository directory {}",
            config.repos_dir.display()
        )
    })?;
    let _lock = lock_name(config, name)?;
    let destination = path_for(config, name);
    if destination.exists() {
        bail!("repository already exists: {}", destination.display());
    }
    let staging = tempfile::Builder::new()
        .prefix(".refuge-create-")
        .tempdir_in(&config.repos_dir)?;
    let staged = staging.path().join("repository.git");
    initialize(&staged)?;
    let id = Uuid::now_v7();
    configure(&staged, id)?;
    std::fs::rename(&staged, &destination)
        .with_context(|| format!("could not publish repository {}", destination.display()))?;
    Ok(Repository {
        path: destination,
        id,
    })
}

pub fn find(config: &Config, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let path = path_for(config, name);
    if !path.is_dir() {
        bail!("repository does not exist: {name}");
    }
    Ok(path)
}

pub fn resolve(config: &Config, selector: &str) -> Result<HostedRepository> {
    if selector == "." {
        return Ok(current(config)?.0);
    }
    let repositories = list(config)?;
    repositories
        .into_iter()
        .find(|repository| repository.name == selector || repository.id.to_string() == selector)
        .with_context(|| format!("repository does not exist: {selector}"))
}

pub fn current(config: &Config) -> Result<(HostedRepository, String)> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    let worktree =
        git::top_level(&cwd).context("current directory is not in a Git working tree")?;
    let repositories = list(config)?;
    let mut matches = Vec::new();
    for (remote, url) in git::remotes(&worktree)? {
        for repository in &repositories {
            if remote_matches(&worktree, &url, &repository.path) {
                matches.push((repository.clone(), remote.clone()));
            }
        }
    }
    matches.sort_by(|left, right| {
        left.0.name.cmp(&right.0.name).then_with(|| {
            remote_priority(&left.1)
                .cmp(&remote_priority(&right.1))
                .then_with(|| left.1.cmp(&right.1))
        })
    });
    matches.dedup_by(|left, right| left.0.id == right.0.id);
    match matches.len() {
        0 => bail!("current Git repository is not connected to a hosted Refuge repository"),
        1 => Ok(matches.pop().expect("one current repository match")),
        _ => bail!("current Git repository is connected to multiple hosted Refuge repositories"),
    }
}

fn remote_priority(name: &str) -> u8 {
    match name {
        "refuge" => 0,
        "origin" => 1,
        _ => 2,
    }
}

pub fn connect(
    config: &Config,
    selector: &str,
    remote_name: &str,
    replace: bool,
) -> Result<(HostedRepository, PathBuf)> {
    let cwd = std::env::current_dir().context("could not read current directory")?;
    connect_at(config, selector, remote_name, replace, &cwd)
}

pub fn connect_at(
    config: &Config,
    selector: &str,
    remote_name: &str,
    replace: bool,
    path: &Path,
) -> Result<(HostedRepository, PathBuf)> {
    let repository = resolve(config, selector)?;
    let worktree = git::top_level(path).context("source is not a Git working tree")?;
    let key = format!("remote.{remote_name}.url");
    match git::config_get_optional(&worktree, &key)? {
        None => git::remote_add(&worktree, remote_name, &repository.path)?,
        Some(url) if remote_matches(&worktree, &url, &repository.path) => {}
        Some(_) if replace => git::remote_set_url(&worktree, remote_name, &repository.path)?,
        Some(_) => bail!(
            "remote `{remote_name}` already exists with a different URL; pass --replace to update it"
        ),
    }
    Ok((repository, worktree))
}

fn remote_matches(worktree: &Path, url: &str, hosted: &Path) -> bool {
    if url == hosted.to_string_lossy() {
        return true;
    }
    let local = url.strip_prefix("file://").unwrap_or(url);
    let path = PathBuf::from(local);
    let path = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    match (dunce::canonicalize(path), dunce::canonicalize(hosted)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub fn destination(config: &Config, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(path_for(config, name))
}

pub fn list(config: &Config) -> Result<Vec<HostedRepository>> {
    let mut repositories = Vec::new();
    for entry in std::fs::read_dir(&config.repos_dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".git") else {
            continue;
        };
        if name.starts_with(".refuge-") {
            continue;
        }
        let id = Uuid::parse_str(&git::config_get(&path, "refuge.repoid")?).with_context(|| {
            format!("repository {} has an invalid refuge.repoid", path.display())
        })?;
        repositories.push(HostedRepository {
            name: name.to_owned(),
            path,
            id,
        });
    }
    repositories.sort_by(|left, right| left.name.cmp(&right.name));
    let mut identities = std::collections::HashMap::new();
    for repository in &repositories {
        if let Some(previous) = identities.insert(repository.id, &repository.name) {
            bail!(
                "repositories {} and {} have duplicate refuge.repoid {}; restore or fork one repository with a new identity",
                previous,
                repository.name,
                repository.id
            );
        }
    }
    Ok(repositories)
}

pub fn ensure_identity_available(config: &Config, id: Uuid, destination: &Path) -> Result<()> {
    if let Some(existing) = list(config)?
        .into_iter()
        .find(|repository| repository.id == id && repository.path != destination)
    {
        bail!(
            "repository identity {id} is already active as {}; `--as` renames a restore and does not create a new identity",
            existing.name
        );
    }
    Ok(())
}

pub(crate) fn lock_name(config: &Config, name: &str) -> Result<std::fs::File> {
    lock_file(
        &config.repos_dir.join(format!(".refuge-name-{name}.lock")),
        "repository name",
    )
}

pub(crate) fn lock_identity(config: &Config, id: Uuid) -> Result<std::fs::File> {
    lock_file(
        &config.repos_dir.join(format!(".refuge-identity-{id}.lock")),
        "repository identity",
    )
}

fn lock_file(path: &Path, description: &str) -> Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::lock_exclusive(&lock).with_context(|| format!("could not lock {description}"))?;
    Ok(lock)
}

fn path_for(config: &Config, name: &str) -> PathBuf {
    config.repos_dir.join(format!("{name}.git"))
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(".git")
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character));
    if !valid {
        bail!(
            "invalid repository name `{name}`: use letters, digits, dots, dashes, or underscores, and omit the `.git` suffix"
        );
    }
    Ok(())
}

pub fn configure(path: &Path, id: Uuid) -> Result<()> {
    git::config_set(path, "refuge.repoid", &id.to_string())?;
    git::config_set(path, "gc.auto", "0")?;
    git::config_set(path, "maintenance.auto", "false")?;
    install_hook(path)?;
    Ok(())
}

pub fn install_hook(repo: &Path) -> Result<()> {
    // Resolve `refuge` through PATH at hook-execution time rather than
    // baking in the absolute path of whichever binary ran this command.
    // This mirrors how pre-commit/prek install their git hooks, and avoids
    // silently breaking backups if the refuge executable is later moved,
    // rebuilt, or upgraded in place. If `refuge` isn't on PATH when the
    // hook runs, fail loudly instead of a silent no-op.
    let hook = "#!/bin/sh\n\
        command -v refuge >/dev/null 2>&1 && exec refuge hook post-receive\n\
        echo \"refuge: 'refuge' not found on PATH; this push was NOT backed up.\" >&2\n\
        echo \"Add refuge's install directory to PATH, or run 'refuge repo backup' manually.\" >&2\n\
        exit 1\n";
    let hook_path = repo.join("hooks").join("post-receive");
    std::fs::write(&hook_path, hook)
        .with_context(|| format!("could not write hook {}", hook_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook_path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook_path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_configuration_never_publishes_a_partial_repository() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            repos_dir: temp.path().join("repos"),
            target_root: temp.path().join("target"),
            instance_id: Uuid::nil(),
        };

        let error = create_staged(&config, "broken", |path| {
            git::init_bare(path)?;
            bail!("injected configuration failure")
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected"));
        assert!(!config.repos_dir.join("broken.git").exists());
        assert!(
            std::fs::read_dir(&config.repos_dir)
                .unwrap()
                .all(|entry| entry.unwrap().path().is_file())
        );
    }
}
