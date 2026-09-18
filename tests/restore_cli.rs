use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::str::contains;
use refuge::git;
use refuge::manifest::Manifest;

/// PATH value with the built `refuge` binary's directory prepended, so the
/// post-receive hook's `command -v refuge` can find it during tests, the
/// same way it would find a real installation on the user's PATH.
fn path_with_refuge() -> std::ffi::OsString {
    let refuge_dir = assert_cmd::cargo::cargo_bin("refuge")
        .parent()
        .expect("refuge binary has a parent directory")
        .to_owned();
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![refuge_dir];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).expect("join PATH entries")
}

fn run_git(repo: &Path, args: &[&str], config: Option<&Path>) -> String {
    let mut command = ProcessCommand::new("git");
    // Newer git defaults to `safe.bareRepository = explicit`, which refuses
    // to auto-detect a bare repository via `-C`. This helper is used
    // against bare hosted/restored repos, so opt back in explicitly.
    command
        .args(["-c", "safe.bareRepository=all"])
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("PATH", path_with_refuge());
    if let Some(config) = config {
        command.env("REFUGE_CONFIG", config);
    }
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

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

fn manifest_count(target: &Path, repo_id: &str) -> usize {
    std::fs::read_dir(
        target
            .join("refuge/v1/repos")
            .join(repo_id)
            .join("snapshots"),
    )
    .unwrap()
    .filter_map(Result::ok)
    .filter(|entry| entry.path().to_string_lossy().ends_with(".manifest.json"))
    .count()
}

#[test]
fn restored_repository_preserves_identity_and_can_back_up_again() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, target) = initialize(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "vault"])
        .assert()
        .success();
    let hosted = repos.join("vault.git");
    let repo_id = run_git(&hosted, &["config", "refuge.repoid"], None);

    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    run_git(&work, &["init", "--initial-branch=main"], None);
    run_git(&work, &["config", "user.name", "Refuge Test"], None);
    run_git(
        &work,
        &["config", "user.email", "refuge@example.invalid"],
        None,
    );
    std::fs::write(work.join("note.md"), "first\n").unwrap();
    run_git(&work, &["add", "note.md"], None);
    run_git(&work, &["commit", "-m", "first"], None);
    run_git(&work, &["tag", "-a", "v1", "-m", "version one"], None);
    run_git(
        &work,
        &["notes", "--ref=refs/notes/review", "add", "-m", "reviewed"],
        None,
    );
    run_git(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        None,
    );
    run_git(&work, &["push", "--mirror", "refuge"], Some(&config));
    let expected = git::ref_state(&hosted).unwrap();
    assert_eq!(manifest_count(&target, &repo_id), 1);

    let snapshots = target
        .join("refuge/v1/repos")
        .join(&repo_id)
        .join("snapshots");
    let manifest_path = std::fs::read_dir(&snapshots)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".manifest.json"))
        .unwrap();
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    let bundle = target
        .join("refuge/v1/repos")
        .join(&repo_id)
        .join(manifest.artifact.unwrap().key);
    let original_bundle = std::fs::read(&bundle).unwrap();
    let mut tampered_bundle = original_bundle.clone();
    tampered_bundle[0] ^= 0xff;
    std::fs::write(&bundle, tampered_bundle).unwrap();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "vault", "--as", "tampered"])
        .assert()
        .failure()
        .stderr(contains("checksum or size differs"));
    assert!(!repos.join("tampered.git").exists());
    std::fs::write(&bundle, original_bundle).unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "vault"])
        .assert()
        .failure()
        .stderr(contains("already exists"));

    std::fs::remove_dir_all(&hosted).unwrap();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "vault"])
        .assert()
        .success()
        .stdout(contains("restored vault"));

    assert_eq!(
        run_git(&hosted, &["config", "refuge.repoid"], None),
        repo_id
    );
    assert_eq!(git::ref_state(&hosted).unwrap(), expected);

    std::fs::write(work.join("note.md"), "first\nsecond\n").unwrap();
    run_git(&work, &["add", "note.md"], None);
    run_git(&work, &["commit", "-m", "second"], None);
    run_git(&work, &["push", "refuge", "main"], Some(&config));
    assert_eq!(manifest_count(&target, &repo_id), 2);
}
