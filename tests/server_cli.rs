mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;

fn start_server(data: &std::path::Path, target: Option<&std::path::Path>) -> std::process::Child {
    let mut command = Command::new(cargo_bin!("refuge"));
    command
        .arg("serve")
        .arg(data)
        .args(["--listen", "127.0.0.1:0"])
        .env("REFUGE_SECRET", "test-owner-key-0123456789")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(target) = target {
        command.arg("--target").arg(target);
    }
    command.spawn().expect("start refuge serve")
}

fn request(address: &str, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
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
    let data = temp.path().join("data");
    let target = temp.path().join("backup");
    let mut server = start_server(&data, Some(&target));
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
    assert!(data.join("config.toml").is_file());
    assert!(data.join("repos").is_dir());
    assert!(target.is_dir());
    stop(server);

    let mut restarted = start_server(&data, None);
    let _ = address(&mut restarted);
    stop(restarted);
}

#[test]
fn serve_refuses_a_second_process_for_the_same_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let target = temp.path().join("backup");
    let mut first = start_server(&data, Some(&target));
    let _ = address(&mut first);

    let output = Command::new(cargo_bin!("refuge"))
        .arg("serve")
        .arg(&data)
        .args(["--listen", "127.0.0.1:0"])
        .env("REFUGE_SECRET", "test-owner-key-0123456789")
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
    let data = temp.path().join("data");
    let target = temp.path().join("backup");
    let mut server = start_server(&data, Some(&target));
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
    assert!(data.join("repos/notes.git").is_dir());
    stop(server);
}
