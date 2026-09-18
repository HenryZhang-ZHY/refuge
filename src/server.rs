use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use axum::{Json, Router, routing::get};
use fs2::FileExt;
use serde::Serialize;
use uuid::Uuid;

use crate::config::Config;

pub struct ServeOptions {
    pub data_root: PathBuf,
    pub target_root: Option<PathBuf>,
    pub listen: SocketAddr,
}

struct ServerLayout {
    root: PathBuf,
    _config: Config,
    _lock: std::fs::File,
}

#[derive(Serialize)]
struct Health<'a> {
    status: &'a str,
}

#[derive(Serialize)]
struct ServerInfo {
    pid: u32,
    address: SocketAddr,
}

pub fn serve(options: ServeOptions) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the server runtime")?;
    runtime.block_on(serve_async(options))
}

async fn serve_async(options: ServeOptions) -> Result<()> {
    let layout = ServerLayout::open(&options.data_root, options.target_root.as_deref())?;
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("could not listen on {}", options.listen))?;
    let address = listener.local_addr()?;
    layout.write_server_info(address)?;

    println!("serving refuge on http://{address}");
    std::io::stdout().flush()?;

    let app = Router::new().route("/healthz", get(|| async { Json(Health { status: "ok" }) }));
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Refuge HTTP server failed")?;
    let _ = std::fs::remove_file(layout.root.join("serve.json"));
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

impl ServerLayout {
    fn open(root: &Path, requested_target: Option<&Path>) -> Result<Self> {
        let root = std::path::absolute(root)
            .with_context(|| format!("could not resolve server data root {}", root.display()))?;
        std::fs::create_dir_all(&root)
            .with_context(|| format!("could not create server data root {}", root.display()))?;
        let root = dunce::canonicalize(&root)
            .with_context(|| format!("could not resolve server data root {}", root.display()))?;

        let lock_path = root.join(".refuge-serve.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        lock.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!(error).context(format!(
                "another Refuge server is already running for {}",
                root.display()
            ))
        })?;

        let config_path = root.join("config.toml");
        let repos = root.join("repos");
        let config = if config_path.exists() {
            let config = Config::load_from(&config_path)?;
            let expected_repos = resolved_directory(&repos)?;
            if config.repos_dir != expected_repos {
                bail!(
                    "server configuration repository directory is {}, expected {}",
                    config.repos_dir.display(),
                    expected_repos.display()
                );
            }
            if let Some(target) = requested_target {
                let target = resolved_directory(target)?;
                if config.target_root != target {
                    bail!(
                        "server is already configured with backup target {}; refusing {}",
                        config.target_root.display(),
                        target.display()
                    );
                }
            }
            config
        } else {
            let target = requested_target.context(
                "--target is required the first time this server data directory is used",
            )?;
            let repos = resolved_directory(&repos)?;
            let target = resolved_directory(target)?;
            let config = Config {
                repos_dir: repos,
                target_root: target,
                instance_id: Uuid::now_v7(),
            };
            config.save_to(&config_path)?;
            config
        };

        Ok(Self {
            root,
            _config: config,
            _lock: lock,
        })
    }

    fn write_server_info(&self, address: SocketAddr) -> Result<()> {
        let path = self.root.join("serve.json");
        let bytes = serde_json::to_vec_pretty(&ServerInfo {
            pid: std::process::id(),
            address,
        })?;
        std::fs::write(&path, bytes)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(())
    }
}

fn resolved_directory(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)
        .with_context(|| format!("could not resolve {}", path.display()))?;
    std::fs::create_dir_all(&path)
        .with_context(|| format!("could not create {}", path.display()))?;
    dunce::canonicalize(&path).with_context(|| format!("could not resolve {}", path.display()))
}
