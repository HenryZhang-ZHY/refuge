//! `git bundle` never captures a repository's Git LFS content store, so
//! Refuge snapshots and restores `lfs/objects` separately. These tests
//! exercise that path directly by writing files into `lfs/objects` using
//! git-lfs's own on-disk layout (content-addressed by sha256, sharded two
//! levels deep) rather than depending on the `git-lfs` binary being
//! installed wherever the test suite runs.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use refuge::manifest::Manifest;
use sha2::{Digest, Sha256};

fn initialize(temp: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let config = temp.path().join("config.toml");
    let repos = temp.path().join("repos");
    let target = temp.path().join("target");
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args([
            "init",
            "--repos",
            repos.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
        ])
        .assert()
        .success();
    (config, repos, target)
}

/// Writes `content` into `<repo>/lfs/objects/<oid[0:2]>/<oid[2:4]>/<oid>`,
/// matching git-lfs's own local content store layout, and returns the oid.
fn write_lfs_object(repo: &Path, content: &[u8]) -> String {
    let oid = format!("{:x}", Sha256::digest(content));
    let path = repo
        .join("lfs")
        .join("objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    oid
}

fn read_manifest(target: &Path, repo_id: &str) -> Manifest {
    let snapshots = target
        .join("refuge/v1/repos")
        .join(repo_id)
        .join("snapshots");
    std::fs::read_dir(&snapshots)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".manifest.json"))
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
        .max_by_key(|manifest: &Manifest| manifest.generation)
        .expect("a manifest was published")
}

fn repo_id_of(repos: &Path, name: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "safe.bareRepository=all"])
        .arg("-C")
        .arg(repos.join(format!("{name}.git")))
        .args(["config", "refuge.repoid"])
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn backup_without_lfs_objects_omits_the_lfs_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let (config, _repos, target) = initialize(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "plain"])
        .assert()
        .success();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "backup", "plain"])
        .assert()
        .success();

    let repo_id = repo_id_of(&_repos, "plain");
    let manifest = read_manifest(&target, &repo_id);
    assert!(manifest.lfs_artifact.is_none());
}

#[test]
fn backup_and_restore_round_trip_lfs_objects() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, target) = initialize(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "vault"])
        .assert()
        .success();
    let hosted = repos.join("vault.git");

    let first = write_lfs_object(&hosted, b"a large attachment, allegedly");
    let second = write_lfs_object(&hosted, b"another large attachment");

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "backup", "vault"])
        .assert()
        .success()
        .stdout(contains("LFS bytes"));

    let repo_id = repo_id_of(&repos, "vault");
    let manifest = read_manifest(&target, &repo_id);
    let lfs_artifact = manifest
        .lfs_artifact
        .expect("an LFS artifact was published");
    assert_eq!(lfs_artifact.format, "lfs-archive");
    let archive_path = target
        .join("refuge/v1/repos")
        .join(&repo_id)
        .join(&lfs_artifact.key);
    let metadata = std::fs::metadata(&archive_path).unwrap();
    assert_eq!(metadata.len(), lfs_artifact.size);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "vault", "--as", "restored"])
        .assert()
        .success();

    let restored = repos.join("restored.git");
    for (oid, content) in [
        (first, &b"a large attachment, allegedly"[..]),
        (second, &b"another large attachment"[..]),
    ] {
        let path = restored
            .join("lfs")
            .join("objects")
            .join(&oid[0..2])
            .join(&oid[2..4])
            .join(&oid);
        assert_eq!(std::fs::read(&path).unwrap(), content);
    }
}

#[test]
fn backup_rejects_a_corrupt_lfs_object() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, _target) = initialize(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "vault"])
        .assert()
        .success();
    let hosted = repos.join("vault.git");

    let oid = write_lfs_object(&hosted, b"original content");
    // Corrupt the object after naming it by oid, so its content hash no
    // longer matches its filename.
    let path = hosted
        .join("lfs")
        .join("objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);
    std::fs::write(&path, b"tampered content").unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "backup", "vault"])
        .assert()
        .failure()
        .stderr(contains("is corrupt"));
}

#[test]
fn restore_rejects_a_tampered_lfs_archive() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, target) = initialize(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "vault"])
        .assert()
        .success();
    let hosted = repos.join("vault.git");
    write_lfs_object(&hosted, b"a large attachment");
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "backup", "vault"])
        .assert()
        .success();

    let repo_id = repo_id_of(&repos, "vault");
    let manifest = read_manifest(&target, &repo_id);
    let lfs_artifact = manifest
        .lfs_artifact
        .expect("an LFS artifact was published");
    let archive_path = target
        .join("refuge/v1/repos")
        .join(&repo_id)
        .join(&lfs_artifact.key);
    let mut bytes = std::fs::read(&archive_path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&archive_path, bytes).unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "vault", "--as", "tampered"])
        .assert()
        .failure()
        .stderr(contains("checksum or size differs"));
    assert!(!repos.join("tampered.git").exists());
}
