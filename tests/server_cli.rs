mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;

const OWNER_SECRET: &str = "test-owner-key-0123456789";

fn start_server(store: &std::path::Path) -> std::process::Child {
    let mut command = Command::new(cargo_bin!("refuge"));
    command
        .arg("serve")
        .arg(store)
        .args(["--listen", "127.0.0.1:0"])
        .env("REFUGE_SECRET", OWNER_SECRET)
        .env("PATH", support::path_with_refuge())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.spawn().expect("start refuge serve")
}

fn request(address: &str, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn git(current_dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(current_dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git")
}

fn create_repository(address: &str, name: &str) {
    let body = format!(r#"{{"name":"{name}"}}"#);
    let create = format!(
        "POST /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-owner-key-0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    assert!(request(address, create.as_bytes()).starts_with("HTTP/1.1 201 Created"));
}

fn repository_json(address: &str, name: &str) -> String {
    request(
        address,
        format!(
            "GET /api/v1/repos/{name} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-owner-key-0123456789\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
}

fn address(child: &mut std::process::Child) -> String {
    let stdout = child.stdout.take().expect("server stdout");
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).expect("read server output") > 0
            && let Some(address) = line.trim().strip_prefix("serving refuge on http://")
        {
            return address.to_owned();
        }
    }
    panic!("server did not report its address");
}

fn stop(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn serve_bootstraps_persistent_state_and_health_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mut server = start_server(&store);
    let server_address = address(&mut server);

    let response = request(
        &server_address,
        b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(
        response.contains("\r\n\r\n{\"status\":\"ok\"}"),
        "{response}"
    );
    assert!(store.join("store.toml").is_file());
    assert!(store.join("repos").is_dir());
    assert!(store.join("backups").is_dir());
    let web = request(
        &server_address,
        b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(web.starts_with("HTTP/1.1 200 OK"), "{web}");
    assert!(web.contains("<title>Refuge</title>"), "{web}");
    stop(server);

    let moved_store = temp.path().join("moved-store");
    std::fs::rename(&store, &moved_store).unwrap();
    let mut restarted = start_server(&moved_store);
    let _ = address(&mut restarted);
    stop(restarted);
}

#[test]
fn serve_requires_an_injected_owner_secret() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");

    let output = Command::new(cargo_bin!("refuge"))
        .arg("serve")
        .arg(&store)
        .args(["--listen", "127.0.0.1:0"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("REFUGE_SECRET_FILE"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_rejects_a_legacy_backup_directory_as_a_store() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    std::fs::create_dir_all(store.join("refuge/v1/repos")).unwrap();

    let output = Command::new(cargo_bin!("refuge"))
        .arg("serve")
        .arg(&store)
        .args(["--listen", "127.0.0.1:0"])
        .env("REFUGE_SECRET", OWNER_SECRET)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("use an empty directory"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_refuses_a_second_process_for_the_same_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mut first = start_server(&store);
    let _ = address(&mut first);

    let output = Command::new(cargo_bin!("refuge"))
        .arg("serve")
        .arg(&store)
        .args(["--listen", "127.0.0.1:0"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stop(first);
}

#[test]
fn repository_api_requires_the_owner_secret_and_creates_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mut server = start_server(&store);
    let address = address(&mut server);

    let unauthorized = request(
        &address,
        b"GET /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(
        unauthorized.starts_with("HTTP/1.1 401 Unauthorized"),
        "{unauthorized}"
    );

    let body = br#"{"name":"notes"}"#;
    let create = format!(
        "POST /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-owner-key-0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let created = request(&address, create.as_bytes());
    assert!(created.starts_with("HTTP/1.1 201 Created"), "{created}");
    assert!(created.contains("\"name\":\"notes\""), "{created}");
    assert!(
        created.contains("\"clone_path\":\"/git/notes.git\""),
        "{created}"
    );

    let listed = request(
        &address,
        b"GET /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-owner-key-0123456789\r\nConnection: close\r\n\r\n",
    );
    assert!(listed.starts_with("HTTP/1.1 200 OK"), "{listed}");
    assert!(listed.contains("\"name\":\"notes\""), "{listed}");
    assert!(store.join("repos/notes.git").is_dir());
    stop(server);
}

#[test]
fn web_session_reuses_the_owner_secret_without_returning_it_to_javascript() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mut server = start_server(&store);
    let address = address(&mut server);
    let body = br#"{"secret":"test-owner-key-0123456789"}"#;
    let login = request(
        &address,
        format!(
            "POST /api/v1/session HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        )
        .as_bytes(),
    );
    assert!(login.starts_with("HTTP/1.1 204 No Content"), "{login}");
    let cookie = login
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .and_then(|line| line.split(';').next())
        .expect("session cookie");
    assert!(!cookie.contains("test-owner-key"));
    assert!(login.contains("HttpOnly"));
    assert!(login.contains("SameSite=Strict"));

    let authenticated = request(
        &address,
        format!(
            "GET /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    );
    assert!(
        authenticated.starts_with("HTTP/1.1 200 OK"),
        "{authenticated}"
    );
    stop(server);
}

#[test]
fn standard_git_clients_clone_push_and_fetch_over_http() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mut server = start_server(&store);
    let address = address(&mut server);
    let body = br#"{"name":"notes"}"#;
    let create = format!(
        "POST /api/v1/repos HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-owner-key-0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    assert!(request(&address, create.as_bytes()).starts_with("HTTP/1.1 201 Created"));

    let remote = format!("http://refuge:test-owner-key-0123456789@{address}/git/notes.git");
    let first = temp.path().join("first");
    let clone = git(temp.path(), &["clone", &remote, first.to_str().unwrap()]);
    assert!(
        clone.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert!(
        git(&first, &["config", "user.name", "Refuge Test"])
            .status
            .success()
    );
    assert!(
        git(&first, &["config", "user.email", "refuge@example.invalid"])
            .status
            .success()
    );
    std::fs::write(first.join("README.md"), "served by Refuge\n").unwrap();
    assert!(git(&first, &["add", "README.md"]).status.success());
    assert!(git(&first, &["commit", "-m", "initial"]).status.success());
    let push = git(&first, &["push", "origin", "main"]);
    assert!(
        push.status.success(),
        "git push failed: {}",
        String::from_utf8_lossy(&push.stderr)
    );

    let second = temp.path().join("second");
    let clone = git(temp.path(), &["clone", &remote, second.to_str().unwrap()]);
    assert!(
        clone.status.success(),
        "second clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(second.join("README.md")).unwrap(),
        "served by Refuge\n"
    );
    stop(server);
}

#[test]
fn server_push_is_accepted_while_backup_is_pending_and_retries_automatically() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let target = store.join("backups");
    let mut server = start_server(&store);
    let address = address(&mut server);
    create_repository(&address, "offline");

    let remote = format!("http://refuge:test-owner-key-0123456789@{address}/git/offline.git");
    let work = temp.path().join("work");
    assert!(
        git(temp.path(), &["clone", &remote, work.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        git(&work, &["config", "user.name", "Refuge Test"])
            .status
            .success()
    );
    assert!(
        git(&work, &["config", "user.email", "refuge@example.invalid"])
            .status
            .success()
    );
    std::fs::write(work.join("pending.txt"), "must survive\n").unwrap();
    assert!(git(&work, &["add", "pending.txt"]).status.success());
    assert!(
        git(&work, &["commit", "-m", "pending backup"])
            .status
            .success()
    );

    let staging = target.join(".refuge-staging");
    let saved_staging = target.join(".refuge-staging.saved");
    std::fs::rename(&staging, &saved_staging).unwrap();
    std::fs::write(&staging, "temporarily unavailable").unwrap();

    let push = git(&work, &["push", "origin", "main"]);
    assert!(
        push.status.success(),
        "push must not depend on backup availability: {}",
        String::from_utf8_lossy(&push.stderr)
    );
    let pending = repository_json(&address, "offline");
    assert!(pending.contains("\"protection\":\"pending\""), "{pending}");

    std::fs::remove_file(&staging).unwrap();
    std::fs::rename(&saved_staging, &staging).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = repository_json(&address, "offline");
        if status.contains("\"protection\":\"protected\"") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "backup was not retried: {status}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    stop(server);
}

#[test]
fn standard_git_lfs_clients_upload_and_download_over_http() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let target = store.join("backups");
    let mut server = start_server(&store);
    let address = address(&mut server);
    create_repository(&address, "media");
    let remote = format!("http://refuge:test-owner-key-0123456789@{address}/git/media.git");
    let first = temp.path().join("lfs-first");
    assert!(
        git(temp.path(), &["clone", &remote, first.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        git(&first, &["config", "user.name", "Refuge Test"])
            .status
            .success()
    );
    assert!(
        git(&first, &["config", "user.email", "refuge@example.invalid"])
            .status
            .success()
    );
    assert!(git(&first, &["lfs", "install", "--local"]).status.success());
    assert!(git(&first, &["lfs", "track", "*.bin"]).status.success());
    let content = b"real lfs content served over standard HTTP\n";
    std::fs::write(first.join("asset.bin"), content).unwrap();
    assert!(
        git(&first, &["add", ".gitattributes", "asset.bin"])
            .status
            .success()
    );
    assert!(
        git(&first, &["commit", "-m", "add LFS asset"])
            .status
            .success()
    );
    let push = git(&first, &["push", "origin", "main"]);
    assert!(
        push.status.success(),
        "Git LFS push failed: {}",
        String::from_utf8_lossy(&push.stderr)
    );

    let second = temp.path().join("lfs-second");
    let clone = git(temp.path(), &["clone", &remote, second.to_str().unwrap()]);
    assert!(
        clone.status.success(),
        "Git LFS clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert_eq!(std::fs::read(second.join("asset.bin")).unwrap(), content);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let archives = std::fs::read_dir(&target)
            .into_iter()
            .flatten()
            .flat_map(|entry| walk_files(entry.unwrap().path()))
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("tar"))
            .count();
        if archives > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "LFS snapshot was not published");
        std::thread::sleep(Duration::from_millis(100));
    }
    stop(server);
}

fn walk_files(path: std::path::PathBuf) -> Vec<std::path::PathBuf> {
    if path.is_file() {
        return vec![path];
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flat_map(|entry| walk_files(entry.unwrap().path()))
        .collect()
}
