use std::collections::BTreeMap;
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
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use fs2::FileExt;
use futures_util::TryStreamExt;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::io::{ReaderStream, StreamReader};
use uuid::Uuid;

use crate::config::Config;
use crate::discovery::ProtectionState;

pub struct ServeOptions {
    pub store_root: PathBuf,
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
    store_root: PathBuf,
    secret: Arc<[u8]>,
    session_token: Arc<str>,
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

#[derive(Deserialize, Serialize)]
struct StoreMetadata {
    schema_version: u32,
    instance_id: Uuid,
}

#[derive(Deserialize)]
struct CreateRepository {
    name: String,
}

#[derive(Deserialize)]
struct LoginRequest {
    secret: String,
}

#[derive(Serialize)]
struct ApiRepository {
    name: String,
    id: Uuid,
    clone_path: String,
    protection: &'static str,
    snapshot_id: Option<String>,
}

#[derive(Deserialize)]
struct LfsBatchRequest {
    operation: String,
    objects: Vec<LfsObjectRequest>,
}

#[derive(Deserialize)]
struct LfsObjectRequest {
    oid: String,
    size: u64,
}

#[derive(Serialize)]
struct LfsBatchResponse {
    transfer: &'static str,
    objects: Vec<LfsObjectResponse>,
    hash_algo: &'static str,
}

#[derive(Serialize)]
struct LfsObjectResponse {
    oid: String,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    authenticated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<LfsActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<LfsObjectError>,
}

#[derive(Serialize)]
struct LfsActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    upload: Option<LfsAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download: Option<LfsAction>,
}

#[derive(Serialize)]
struct LfsAction {
    href: String,
    header: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct LfsObjectError {
    code: u16,
    message: String,
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

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct WebAssets;

pub fn serve(options: ServeOptions) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the server runtime")?;
    runtime.block_on(serve_async(options))
}

async fn serve_async(options: ServeOptions) -> Result<()> {
    let layout = ServerLayout::open(&options.store_root)?;
    let secret = load_secret(&layout.root)?;
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("could not listen on {}", options.listen))?;
    let address = listener.local_addr()?;
    layout.write_server_info(address)?;
    let state = AppState {
        config: layout.config.clone(),
        store_root: layout.root.clone(),
        session_token: session_token(&secret).into(),
        secret,
    };
    crate::backup_queue::reconcile(&state.config, &layout.root)?;
    spawn_backup_worker(state.config.clone(), layout.root.clone());

    println!("serving refuge on http://{address}");
    std::io::stdout().flush()?;

    let protected_api = Router::new()
        .route("/repos", get(list_repositories).post(create_repository))
        .route("/repos/{name}", get(view_repository))
        .layer(middleware::from_fn_with_state(state.clone(), require_owner));
    let api = Router::new()
        .route("/session", post(login).delete(logout))
        .merge(protected_api);
    let git = Router::new()
        .route("/{repo}/info/lfs/objects/batch", post(lfs_batch))
        .route(
            "/{repo}/info/lfs/objects/{oid}",
            get(lfs_download).put(lfs_upload),
        )
        .fallback(git_http)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_git_owner,
        ));
    let app = Router::new()
        .route("/healthz", get(|| async { Json(Health { status: "ok" }) }))
        .nest("/api/v1", api)
        .nest("/git", git)
        .fallback(web_asset)
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

async fn web_asset(request: Request) -> Response {
    let requested = request.uri().path().trim_start_matches('/');
    let path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let asset = WebAssets::get(path).or_else(|| WebAssets::get("index.html"));
    let Some(asset) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .as_ref()
        .to_owned();
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache_control.to_owned()),
        ],
        asset.data,
    )
        .into_response()
}

impl ServerLayout {
    fn open(root: &Path) -> Result<Self> {
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

        initialize_store(&root)?;
        let config = load_store_config(&root)?;

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

fn initialize_store(root: &Path) -> Result<()> {
    let metadata_path = root.join("store.toml");
    if !metadata_path.exists() {
        reject_nonempty_uninitialized_store(root)?;
    }

    resolved_directory(&root.join("repos"))?;
    resolved_directory(&root.join("backups"))?;
    resolved_directory(&root.join("queue"))?;
    resolved_directory(&root.join("secrets"))?;

    if metadata_path.exists() {
        return Ok(());
    }
    let metadata = StoreMetadata {
        schema_version: 1,
        instance_id: Uuid::now_v7(),
    };
    let text = toml::to_string_pretty(&metadata).context("could not serialize store metadata")?;
    let mut partial = tempfile::Builder::new()
        .prefix(".refuge-store-")
        .tempfile_in(root)
        .with_context(|| format!("could not stage store metadata in {}", root.display()))?;
    partial.write_all(text.as_bytes())?;
    partial.as_file().sync_all()?;
    partial.persist_noclobber(&metadata_path).map_err(|error| {
        anyhow::anyhow!(error.error)
            .context(format!("could not create {}", metadata_path.display()))
    })?;
    sync_directory(root)?;
    Ok(())
}

fn reject_nonempty_uninitialized_store(root: &Path) -> Result<()> {
    let unexpected = std::fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        .find(|name| name != ".refuge-serve.lock" && name != "secrets");
    if let Some(name) = unexpected {
        bail!(
            "{} is not an initialized Refuge store and contains unexpected entry {}; use an empty directory",
            root.display(),
            name.to_string_lossy()
        );
    }
    Ok(())
}

pub fn load_store_config(root: &Path) -> Result<Config> {
    let root = dunce::canonicalize(root)
        .with_context(|| format!("could not resolve Refuge store {}", root.display()))?;
    let metadata_path = root.join("store.toml");
    let text = std::fs::read_to_string(&metadata_path)
        .with_context(|| format!("could not read {}", metadata_path.display()))?;
    let metadata: StoreMetadata =
        toml::from_str(&text).with_context(|| format!("invalid {}", metadata_path.display()))?;
    if metadata.schema_version != 1 {
        bail!(
            "unsupported Refuge store schema version {}",
            metadata.schema_version
        );
    }
    Ok(Config {
        repos_dir: resolved_existing_directory(&root.join("repos"))?,
        target_root: resolved_existing_directory(&root.join("backups"))?,
        instance_id: metadata.instance_id,
    })
}

fn resolved_directory(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)
        .with_context(|| format!("could not resolve {}", path.display()))?;
    std::fs::create_dir_all(&path)
        .with_context(|| format!("could not create {}", path.display()))?;
    dunce::canonicalize(&path).with_context(|| format!("could not resolve {}", path.display()))
}

fn resolved_existing_directory(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).with_context(|| format!("could not resolve {}", path.display()))
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

fn load_secret(store_root: &Path) -> Result<Arc<[u8]>> {
    let path = store_root.join("secrets/owner-secret");
    let mut secret = std::fs::read(&path).with_context(|| {
        format!(
            "could not read {}; create it with at least 16 bytes before starting Refuge",
            path.display()
        )
    })?;
    while secret
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        secret.pop();
    }
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
    let bearer_authenticated = provided.is_some_and(|provided| {
        provided.len() == state.secret.len() && bool::from(provided.ct_eq(state.secret.as_ref()))
    });
    let cookie_authenticated = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix("refuge_session=")
                    .map(str::as_bytes)
            })
        })
        .is_some_and(|provided| {
            provided.len() == state.session_token.len()
                && bool::from(provided.ct_eq(state.session_token.as_bytes()))
        });
    if bearer_authenticated || cookie_authenticated {
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

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let provided = request.secret.as_bytes();
    if provided.len() != state.secret.len() || !bool::from(provided.ct_eq(state.secret.as_ref())) {
        return Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "the owner key is incorrect".to_owned(),
        });
    }
    let cookie = format!(
        "refuge_session={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800",
        state.session_token
    );
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap())],
    )
        .into_response())
}

async fn logout() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            "refuge_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        )],
    )
        .into_response()
}

fn session_token(secret: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"refuge-web-session-v1\0");
    digest.update(secret);
    format!("{:x}", digest.finalize())
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

async fn lfs_batch(
    State(state): State<AppState>,
    axum::extract::Path(repo_segment): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    Json(batch): Json<LfsBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let repository = lfs_repository(&state.config, &repo_segment)?;
    if batch.operation != "upload" && batch.operation != "download" {
        return Err(ApiError::bad_request(format!(
            "unsupported LFS operation {}",
            batch.operation
        )));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("Host header is required".to_owned()))?;
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| matches!(*value, "http" | "https"))
        .unwrap_or("http");
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut action_headers = BTreeMap::new();
    action_headers.insert("Authorization".to_owned(), authorization);
    let mut objects = Vec::with_capacity(batch.objects.len());
    for object in batch.objects {
        validate_lfs_oid(&object.oid)?;
        let path = lfs_object_path(&repository.path, &object.oid);
        let action = LfsAction {
            href: format!(
                "{scheme}://{host}/git/{repo_segment}/info/lfs/objects/{}",
                object.oid
            ),
            header: action_headers.clone(),
        };
        let (authenticated, actions, error) = match batch.operation.as_str() {
            "upload" if valid_existing_lfs_object(&path, &object.oid, object.size) => {
                (None, None, None)
            }
            "upload" => (
                Some(true),
                Some(LfsActions {
                    upload: Some(action),
                    download: None,
                }),
                None,
            ),
            "download" if valid_existing_lfs_object(&path, &object.oid, object.size) => (
                Some(true),
                Some(LfsActions {
                    upload: None,
                    download: Some(action),
                }),
                None,
            ),
            "download" => (
                None,
                None,
                Some(LfsObjectError {
                    code: 404,
                    message: "LFS object does not exist".to_owned(),
                }),
            ),
            _ => unreachable!(),
        };
        objects.push(LfsObjectResponse {
            oid: object.oid,
            size: object.size,
            authenticated,
            actions,
            error,
        });
    }
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.git-lfs+json")],
        Json(LfsBatchResponse {
            transfer: "basic",
            objects,
            hash_algo: "sha256",
        }),
    ))
}

async fn lfs_upload(
    State(state): State<AppState>,
    axum::extract::Path((repo_segment, oid)): axum::extract::Path<(String, String)>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    validate_lfs_oid(&oid)?;
    let repository = lfs_repository(&state.config, &repo_segment)?;
    let incoming = repository.path.join("lfs").join("incoming");
    tokio::fs::create_dir_all(&incoming)
        .await
        .map_err(|error| ApiError::internal(format!("could not create LFS staging: {error}")))?;
    let staged = incoming.join(Uuid::now_v7().to_string());
    let mut output = tokio::fs::File::create(&staged)
        .await
        .map_err(|error| ApiError::internal(format!("could not stage LFS object: {error}")))?;
    let stream = request
        .into_body()
        .into_data_stream()
        .map_err(std::io::Error::other);
    let mut input = StreamReader::new(stream);
    tokio::io::copy(&mut input, &mut output)
        .await
        .map_err(|error| ApiError::internal(format!("could not upload LFS object: {error}")))?;
    output
        .sync_all()
        .await
        .map_err(|error| ApiError::internal(format!("could not flush LFS object: {error}")))?;
    drop(output);

    let staged_for_check = staged.clone();
    let (checksum, _) =
        tokio::task::spawn_blocking(move || crate::storage::checksum(&staged_for_check))
            .await
            .map_err(|error| ApiError::internal(format!("LFS checksum task failed: {error}")))?
            .map_err(ApiError::internal)?;
    if checksum != format!("sha256:{oid}") {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(ApiError::unprocessable(
            "uploaded LFS object does not match its oid".to_owned(),
        ));
    }

    let destination = lfs_object_path(&repository.path, &oid);
    let parent = destination
        .parent()
        .ok_or_else(|| ApiError::internal("LFS object path has no parent"))?;
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        ApiError::internal(format!("could not create LFS object path: {error}"))
    })?;
    if destination.exists() {
        let _ = tokio::fs::remove_file(&staged).await;
    } else {
        tokio::fs::rename(&staged, &destination)
            .await
            .map_err(|error| {
                ApiError::internal(format!("could not publish LFS object: {error}"))
            })?;
    }
    Ok(StatusCode::OK)
}

async fn lfs_download(
    State(state): State<AppState>,
    axum::extract::Path((repo_segment, oid)): axum::extract::Path<(String, String)>,
) -> Result<Response, ApiError> {
    validate_lfs_oid(&oid)?;
    let repository = lfs_repository(&state.config, &repo_segment)?;
    let path = lfs_object_path(&repository.path, &oid);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("LFS object does not exist".to_owned()))?;
    let size = file.metadata().await.map_err(ApiError::internal)?.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size)
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(ApiError::internal)
}

fn lfs_repository(
    config: &Config,
    segment: &str,
) -> Result<crate::repo::HostedRepository, ApiError> {
    let name = segment
        .strip_suffix(".git")
        .ok_or_else(|| ApiError::not_found("Git repository does not exist".to_owned()))?;
    let repository = crate::repo::resolve(config, name)
        .map_err(|_| ApiError::not_found("Git repository does not exist".to_owned()))?;
    if repository.name != name {
        return Err(ApiError::not_found(
            "Git repository does not exist".to_owned(),
        ));
    }
    Ok(repository)
}

fn validate_lfs_oid(oid: &str) -> Result<(), ApiError> {
    if oid.len() != 64 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("invalid LFS oid".to_owned()));
    }
    Ok(())
}

fn lfs_object_path(repository: &Path, oid: &str) -> PathBuf {
    repository
        .join("lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

fn valid_existing_lfs_object(path: &Path, oid: &str, expected_size: u64) -> bool {
    let Ok((checksum, size)) = crate::storage::checksum(path) else {
        return false;
    };
    size == expected_size && checksum == format!("sha256:{oid}")
}

async fn git_http(State(state): State<AppState>, request: Request) -> Result<Response, ApiError> {
    let path = request.uri().path().trim_start_matches('/').to_owned();
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
        .env("REFUGE_SERVER_STORE", &state.store_root)
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

    fn unprocessable(message: String) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_lfs_object",
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

fn spawn_backup_worker(config: Config, store_root: PathBuf) {
    tokio::spawn(async move {
        loop {
            let config = config.clone();
            let store_root = store_root.clone();
            match tokio::task::spawn_blocking(move || {
                crate::backup_queue::process_once(&config, &store_root)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("refuge: backup worker failed: {error:#}"),
                Err(error) => eprintln!("refuge: backup worker stopped unexpectedly: {error}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}
