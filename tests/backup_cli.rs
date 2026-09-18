use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use refuge::manifest::Manifest;

fn git(repo: &Path, args: &[&str], config: Option<&Path>) -> String {
    let mut command = ProcessCommand::new("git");
    command.arg("-C").arg(repo).args(args);
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

fn initialized(temp: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
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

fn manifests(target: &Path, repo_id: &str) -> Vec<PathBuf> {
    let directory = target
        .join("refuge/v1/repos")
        .join(repo_id)
        .join("snapshots");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".manifest.json"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn push_publishes_verified_bundle_then_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, target) = initialized(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "ledger"])
        .assert()
        .success();
    let hosted = repos.join("ledger.git");
    let repo_id = git(&hosted, &["config", "refuge.repoid"], None);

    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    git(&work, &["init", "--initial-branch=main"], None);
    git(&work, &["config", "user.name", "Refuge Test"], None);
    git(
        &work,
        &["config", "user.email", "refuge@example.invalid"],
        None,
    );
    std::fs::write(work.join("ledger.bean"), "2026-01-01 open Assets:Cash\n").unwrap();
    git(&work, &["add", "ledger.bean"], None);
    git(&work, &["commit", "-m", "add ledger"], None);
    git(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        None,
    );
    git(&work, &["push", "refuge", "main"], Some(&config));

    let paths = manifests(&target, &repo_id);
    assert_eq!(paths.len(), 1);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.repo_name, "ledger");
    assert_eq!(manifest.generation, 1);
    assert!(manifest.refs.contains_key("refs/heads/main"));
    let artifact = manifest.artifact.expect("bundle artifact");
    let bundle = target
        .join("refuge/v1/repos")
        .join(repo_id)
        .join(artifact.key);
    assert_eq!(std::fs::metadata(&bundle).unwrap().len(), artifact.size);
    refuge::git::bundle_verify(&hosted, &bundle).unwrap();
}

#[test]
fn empty_repository_backup_publishes_manifest_without_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos, target) = initialized(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "empty"])
        .assert()
        .success();
    let hosted = repos.join("empty.git");
    let repo_id = git(&hosted, &["config", "refuge.repoid"], None);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["backup", "empty"])
        .assert()
        .success();

    let path = manifests(&target, &repo_id).pop().unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest.refs.len(), 1);
    assert_eq!(manifest.head(), Some("refs/heads/main"));
    assert!(manifest.artifact.is_none());
}
