use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::{config::Config, git};

pub struct Repository {
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
        bail!("invalid repository name: {name}");
    }
    Ok(())
}

fn finish_setup(path: PathBuf) -> Result<Repository> {
    let id = Uuid::now_v7();
    git::config_set(&path, "refuge.repoid", &id.to_string())?;
    git::config_set(&path, "gc.auto", "0")?;
    git::config_set(&path, "maintenance.auto", "false")?;
    install_hook(&path)?;
    Ok(Repository { path, id })
}

pub fn install_hook(repo: &Path) -> Result<()> {
    let executable = std::env::current_exe().context("could not locate the refuge executable")?;
    let executable = executable
        .to_str()
        .context("refuge executable path is not valid UTF-8")?;
    let quoted = format!("'{}'", executable.replace('\'', "'\"'\"'"));
    let hook = format!("#!/bin/sh\nexec {quoted} hook post-receive\n");
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
