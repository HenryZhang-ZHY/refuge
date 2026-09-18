use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use base64::Engine;
use fs2::FileExt;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::io::{ReaderStream, StreamReader};
use uuid::Uuid;

use crate::config::Config;
use crate::discovery::ProtectionState;

pub struct ServeOptions {
    pub data_root: PathBuf,
    pub target_root: Option<PathBuf>,
    pub listen: SocketAddr,
}

struct ServerLayout {
    root: PathBuf,
    config: Config,
    _lock: std::fs::File,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    config_path: PathBuf,
    secret: Arc<[u8]>,
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

#[derive(Deserialize)]
struct CreateRepository {
    name: String,
}

#[derive(Serialize)]
struct ApiRepository {
    name: String,
    id: Uuid,
    clone_path: String,
    protection: &'static str,
    snapshot_id: Option<String>,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

pub fn serve(options: ServeOptions) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the server runtime")?;
    runtime.block_on(serve_async(options))
}

async fn serve_async(options: ServeOptions) -> Result<()> {
    let layout = ServerLayout::open(&options.data_root, options.target_root.as_deref())?;
    let secret = load_secret()?;
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("could not listen on {}", options.listen))?;
    let address = listener.local_addr()?;
    layout.write_server_info(address)?;
    let state = AppState {
        config: layout.config.clone(),
        config_path: layout.root.join("config.toml"),
        secret,
    };

    println!("serving refuge on http://{address}");
    std::io::stdout().flush()?;

    let api = Router::new()
        .route("/repos", get(list_repositories).post(create_repository))
        .route("/repos/{name}", get(view_repository))
        .layer(middleware::from_fn_with_state(state.clone(), require_owner));
    let app = Router::new()
        .route("/healthz", get(|| async { Json(Health { status: "ok" }) }))
        .nest("/api/v1", api)
        .route(
            "/git/{*path}",
            any(git_http).layer(middleware::from_fn_with_state(
                state.clone(),
                require_git_owner,
            )),
        )
        .with_state(state);
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
            config,
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

fn load_secret() -> Result<Arc<[u8]>> {
    let secret = if let Some(path) = std::env::var_os("REFUGE_SECRET_FILE") {
        let path = PathBuf::from(path);
        let mut bytes = std::fs::read(&path)
            .with_context(|| format!("could not read REFUGE_SECRET_FILE {}", path.display()))?;
        while bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            bytes.pop();
        }
        bytes
    } else if let Some(secret) = std::env::var_os("REFUGE_SECRET") {
        secret.to_string_lossy().as_bytes().to_vec()
    } else {
        bail!("set REFUGE_SECRET_FILE (recommended) or REFUGE_SECRET before starting the server");
    };
    if secret.len() < 16 {
        bail!("the Refuge server secret must contain at least 16 bytes");
    }
    Ok(secret.into())
}

async fn require_owner(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::as_bytes);
    let authenticated = provided.is_some_and(|provided| {
        provided.len() == state.secret.len() && bool::from(provided.ct_eq(state.secret.as_ref()))
    });
    if authenticated {
        return next.run(request).await;
    }
    let mut response = ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "unauthorized",
        message: "a valid owner token is required".to_owned(),
    }
    .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer realm=\"Refuge\""),
    );
    response
}

async fn require_git_owner(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if basic_secret(request.headers(), &state.secret) {
        return next.run(request).await;
    }
    let mut response = ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "unauthorized",
        message: "Git credentials are required".to_owned(),
    }
    .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Refuge Git\", charset=\"UTF-8\""),
    );
    response
}

fn basic_secret(headers: &axum::http::HeaderMap, expected: &[u8]) -> bool {
    let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
    else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Some(separator) = decoded.iter().position(|byte| *byte == b':') else {
        return false;
    };
    let username = &decoded[..separator];
    let password = &decoded[separator + 1..];
    username == b"refuge"
        && password.len() == expected.len()
        && bool::from(password.ct_eq(expected))
}

async fn git_http(
    State(state): State<AppState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let repository_segment = path.split('/').next().unwrap_or_default();
    let repository_name = repository_segment
        .strip_suffix(".git")
        .ok_or_else(|| ApiError::not_found("Git repository does not exist".to_owned()))?;
    let repository = crate::repo::resolve(&state.config, repository_name)
        .map_err(|_| ApiError::not_found("Git repository does not exist".to_owned()))?;
    if repository.name != repository_name {
        return Err(ApiError::not_found(
            "Git repository does not exist".to_owned(),
        ));
    }

    let (parts, body) = request.into_parts();
    let mut command = tokio::process::Command::new("git");
    command
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", &state.config.repos_dir)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", format!("/{path}"))
        .env("REQUEST_METHOD", parts.method.as_str())
        .env("QUERY_STRING", parts.uri.query().unwrap_or_default())
        .env("REMOTE_USER", "refuge")
        .env("REMOTE_ADDR", "unknown")
        .env("REFUGE_CONFIG", &state.config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(value) = parts.headers.get(header::CONTENT_TYPE) {
        command.env("CONTENT_TYPE", value.to_str().unwrap_or_default());
    }
    if let Some(value) = parts.headers.get(header::CONTENT_LENGTH) {
        command.env("CONTENT_LENGTH", value.to_str().unwrap_or_default());
    }
    if let Some(value) = parts.headers.get("git-protocol") {
        command.env("HTTP_GIT_PROTOCOL", value.to_str().unwrap_or_default());
    }

    let mut child = command.spawn().map_err(|error| {
        ApiError::internal(format!("could not start git http-backend: {error}"))
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ApiError::internal("git http-backend stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::internal("git http-backend stdout is unavailable"))?;

    let stream = body.into_data_stream().map_err(std::io::Error::other);
    let mut reader = StreamReader::new(stream);
    tokio::io::copy(&mut reader, &mut stdin)
        .await
        .map_err(|error| ApiError::internal(format!("could not stream Git request: {error}")))?;
    stdin
        .shutdown()
        .await
        .map_err(|error| ApiError::internal(format!("could not finish Git request: {error}")))?;
    drop(stdin);

    cgi_response(stdout, child).await
}

async fn cgi_response(
    stdout: tokio::process::ChildStdout,
    mut child: tokio::process::Child,
) -> Result<Response, ApiError> {
    let mut stdout = BufReader::new(stdout);
    let mut status = StatusCode::OK;
    let mut headers = Vec::new();
    let mut header_bytes = 0_usize;
    loop {
        let mut line = Vec::new();
        let read = stdout
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| ApiError::internal(format!("could not read Git response: {error}")))?;
        if read == 0 {
            return Err(ApiError::internal(
                "git http-backend ended before returning CGI headers",
            ));
        }
        header_bytes += read;
        if header_bytes > 64 * 1024 {
            return Err(ApiError::internal(
                "git http-backend returned oversized headers",
            ));
        }
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            return Err(ApiError::internal(
                "git http-backend returned a malformed header",
            ));
        };
        let name = HeaderName::from_bytes(&line[..separator])
            .map_err(|_| ApiError::internal("git http-backend returned an invalid header name"))?;
        let value = line[separator + 1..]
            .strip_prefix(b" ")
            .unwrap_or(&line[separator + 1..]);
        if name == "status" {
            status = std::str::from_utf8(value)
                .ok()
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u16>().ok())
                .and_then(|value| StatusCode::from_u16(value).ok())
                .ok_or_else(|| ApiError::internal("git http-backend returned an invalid status"))?;
        } else if name != header::CONNECTION && name != header::TRANSFER_ENCODING {
            let value = HeaderValue::from_bytes(value)
                .map_err(|_| ApiError::internal("git http-backend returned an invalid header"))?;
            headers.push((name, value));
        }
    }

    tokio::spawn(async move {
        if let Err(error) = child.wait().await {
            eprintln!("refuge: git http-backend wait failed: {error}");
        }
    });
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from_stream(ReaderStream::new(stdout)))
        .map_err(|error| ApiError::internal(format!("could not build Git response: {error}")))
}

async fn list_repositories(
    State(state): State<AppState>,
) -> Result<Json<Vec<ApiRepository>>, ApiError> {
    let repositories = crate::repo::list(&state.config).map_err(ApiError::internal)?;
    let mut result = Vec::with_capacity(repositories.len());
    for repository in repositories {
        result.push(api_repository(&state.config, repository)?);
    }
    Ok(Json(result))
}

async fn create_repository(
    State(state): State<AppState>,
    Json(request): Json<CreateRepository>,
) -> Result<(StatusCode, Json<ApiRepository>), ApiError> {
    let provisioned = crate::application::create_repository(&state.config, &request.name)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let repository =
        crate::repo::resolve(&state.config, &request.name).map_err(ApiError::internal)?;
    debug_assert_eq!(repository.id, provisioned.repository.id);
    Ok((
        StatusCode::CREATED,
        Json(api_repository(&state.config, repository)?),
    ))
}

async fn view_repository(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<ApiRepository>, ApiError> {
    let repository = crate::repo::resolve(&state.config, &name)
        .map_err(|error| ApiError::not_found(error.to_string()))?;
    Ok(Json(api_repository(&state.config, repository)?))
}

fn api_repository(
    config: &Config,
    repository: crate::repo::HostedRepository,
) -> Result<ApiRepository, ApiError> {
    let state =
        crate::discovery::repository_status(config, &repository).map_err(ApiError::internal)?;
    let (protection, snapshot_id) = match state {
        ProtectionState::Protected { snapshot_id } => ("protected", Some(snapshot_id)),
        ProtectionState::Pending => ("pending", None),
        ProtectionState::Unprotected => ("unprotected", None),
        ProtectionState::Corrupt { .. } => ("corrupt", None),
    };
    Ok(ApiRepository {
        clone_path: format!("/git/{}.git", repository.name),
        name: repository.name,
        id: repository.id,
        protection,
        snapshot_id,
    })
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
        }
    }

    fn not_found(message: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message,
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}
