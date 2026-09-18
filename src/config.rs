use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub repos_dir: PathBuf,
    pub target_root: PathBuf,
    pub instance_id: Uuid,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&default_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("could not serialize config")?;
        std::fs::write(path, text)
            .with_context(|| format!("could not write config {}", path.display()))
    }
}

pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("REFUGE_CONFIG") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(windows)]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.map(|path| path.join("refuge").join("config.toml"))
        .context("could not determine the user config directory")
}

pub fn initialize(repos: Option<PathBuf>, target: Option<PathBuf>) -> Result<(Config, PathBuf)> {
    let config_path = default_path()?;
    let base = config_path
        .parent()
        .context("config path has no parent directory")?;
    let repos = absolute(repos.unwrap_or_else(|| base.join("repos")))?;
    let target = absolute(target.unwrap_or_else(|| base.join("target")))?;
    validate_paths(&repos, &target)?;

    std::fs::create_dir_all(&repos)
        .with_context(|| format!("could not create repository directory {}", repos.display()))?;
    std::fs::create_dir_all(&target)
        .with_context(|| format!("could not create target directory {}", target.display()))?;
    let repos = repos
        .canonicalize()
        .with_context(|| format!("could not resolve repository directory {}", repos.display()))?;
    let target = target
        .canonicalize()
        .with_context(|| format!("could not resolve target directory {}", target.display()))?;
    validate_paths(&repos, &target)?;

    let config = Config {
        repos_dir: repos,
        target_root: target,
        instance_id: Uuid::now_v7(),
    };
    config.save_to(&config_path)?;
    Ok((config, config_path))
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    std::path::absolute(&path).with_context(|| format!("could not resolve path {}", path.display()))
}

fn validate_paths(repos: &Path, target: &Path) -> Result<()> {
    if repos.starts_with(target) {
        bail!("repository directory must not be inside the backup target");
    }
    if target.starts_with(repos) {
        bail!("backup target must not be inside the repository directory");
    }
    for (key, value) in env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("onedrive")
        {
            let root = absolute(PathBuf::from(value))?;
            if repos.starts_with(root) {
                bail!("repository directory must not be inside a OneDrive directory");
            }
        }
    }
    Ok(())
}
