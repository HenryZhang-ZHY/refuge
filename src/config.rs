use std::env;
use std::io::Write;
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
        let path = default_path()?;
        if !path.exists() {
            bail!(
                "Refuge is not initialized: config not found at {}. Run `refuge init --repos <LOCAL_DIR> --target <SYNC_DIR>`.",
                path.display()
            );
        }
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read config {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        if !config.repos_dir.is_absolute() || !config.target_root.is_absolute() {
            bail!("configuration paths must be absolute");
        }
        validate_paths(&config.repos_dir, &config.target_root)?;
        Ok(config)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("could not serialize config")?;
        let parent = path.parent().context("config path has no parent")?;
        let mut partial = tempfile::Builder::new()
            .prefix(".refuge-config-")
            .tempfile_in(parent)
            .with_context(|| format!("could not stage config in {}", parent.display()))?;
        partial.write_all(text.as_bytes())?;
        partial.as_file().sync_all()?;
        partial.persist_noclobber(path).map_err(|error| {
            anyhow::anyhow!(error.error)
                .context(format!("could not create config {}", path.display()))
        })?;
        sync_parent(parent)?;
        Ok(())
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

/// Default location for hosted bare repositories: the XDG *data* directory
/// (`$XDG_DATA_HOME`, `~/.local/share` on Unix; `%LOCALAPPDATA%` on
/// Windows), not the *config* directory. Repositories are local, potentially
/// large, machine-specific data, not configuration, and `%LOCALAPPDATA%` is
/// non-roaming (unlike the `%APPDATA%` used for `config.toml`), which keeps
/// them from being swept into profile roaming/sync mechanisms.
pub fn default_repos_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
    });

    base.map(|path| path.join("refuge").join("repos"))
        .context("could not determine the user data directory")
}

pub fn initialize(repos: Option<PathBuf>, target: Option<PathBuf>) -> Result<(Config, PathBuf)> {
    let config_path = default_path()?;
    let parent = config_path
        .parent()
        .context("config path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let lock_path = parent.join(".refuge-init.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock).context("could not lock Refuge initialization")?;
    if config_path.exists() {
        bail!(
            "Refuge is already initialized at {}; existing configuration was not changed. Remove that file only when intentionally creating a new Refuge instance.",
            config_path.display()
        );
    }
    let base = parent;
    let repos = match repos {
        Some(repos) => absolute(repos)?,
        None => absolute(default_repos_dir()?)?,
    };
    let target = absolute(target.unwrap_or_else(|| base.join("target")))?;
    validate_paths(&repos, &target)?;

    std::fs::create_dir_all(&repos)
        .with_context(|| format!("could not create repository directory {}", repos.display()))?;
    std::fs::create_dir_all(&target)
        .with_context(|| format!("could not create target directory {}", target.display()))?;
    // `dunce::canonicalize` behaves like `Path::canonicalize` but avoids the
    // `\\?\` extended-length prefix on Windows when a normal path suffices;
    // the Git CLI mishandles that prefix for some operations (e.g. `clone`).
    let repos = dunce::canonicalize(&repos)
        .with_context(|| format!("could not resolve repository directory {}", repos.display()))?;
    let target = dunce::canonicalize(&target)
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

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_config_creation_never_overwrites_instance_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let configs: Vec<_> = [Uuid::now_v7(), Uuid::now_v7()]
            .into_iter()
            .map(|instance_id| Config {
                repos_dir: temp.path().join(format!("repos-{instance_id}")),
                target_root: temp.path().join(format!("target-{instance_id}")),
                instance_id,
            })
            .collect();
        let handles: Vec<_> = configs
            .clone()
            .into_iter()
            .map(|config| {
                let path = path.clone();
                std::thread::spawn(move || config.save_to(&path))
            })
            .collect();
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(Result::is_ok)
            .count();

        assert_eq!(successes, 1);
        let loaded = Config::load_from(&path).unwrap();
        assert!(configs.contains(&loaded));
    }

    #[test]
    fn loading_revalidates_path_separation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let config = Config {
            repos_dir: temp.path().join("data"),
            target_root: temp.path().join("data/target"),
            instance_id: Uuid::nil(),
        };
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        assert!(
            Config::load_from(&path)
                .unwrap_err()
                .to_string()
                .contains("inside")
        );
    }
}
