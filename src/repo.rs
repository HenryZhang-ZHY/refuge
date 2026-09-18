use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::{config::Config, git};

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
    validate_name(name)?;
    let path = path_for(config, name);
    if path.exists() {
        bail!("repository already exists: {}", path.display());
    }
    git::init_bare(&path)?;
    finish_setup(path)
}

pub fn import(config: &Config, name: &str, source: &Path) -> Result<Repository> {
    validate_name(name)?;
    let path = path_for(config, name);
    if path.exists() {
        bail!("repository already exists: {}", path.display());
    }
    git::clone_mirror(source, &path)?;
    finish_setup(path)
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
    let repositories = list(config)?;
    repositories
        .into_iter()
        .find(|repository| repository.name == selector || repository.id.to_string() == selector)
        .with_context(|| format!("repository does not exist: {selector}"))
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
    Ok(repositories)
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

fn finish_setup(path: PathBuf) -> Result<Repository> {
    let id = Uuid::now_v7();
    configure(&path, id)?;
    Ok(Repository { path, id })
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
        echo \"Add refuge's install directory to PATH, or run 'refuge backup' manually.\" >&2\n\
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
