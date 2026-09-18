use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::str::contains;
use refuge::git;

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn initialized(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let config = temp.path().join("config.toml");
    let repos = temp.path().join("repos");
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args([
            "init",
            "--repos",
            repos.to_str().unwrap(),
            "--target",
            temp.path().join("target").to_str().unwrap(),
        ])
        .assert()
        .success();
    (config, repos)
}

#[test]
fn repo_create_configures_bare_repository_and_hook() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "ledger"])
        .assert()
        .success()
        .stdout(contains("git remote add refuge"));

    let repo = repos.join("ledger.git");
    assert_eq!(
        git_output(&repo, &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    assert_eq!(git_output(&repo, &["config", "gc.auto"]), "0");
    assert_eq!(git_output(&repo, &["config", "maintenance.auto"]), "false");
    uuid::Uuid::parse_str(&git_output(&repo, &["config", "refuge.repoid"])).unwrap();
    let hook = std::fs::read_to_string(repo.join("hooks/post-receive")).unwrap();
    assert!(hook.contains("hook post-receive"));
}

#[test]
fn repo_import_preserves_all_refs() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git_output(&source, &["init", "--initial-branch=main"]);
    git_output(&source, &["config", "user.name", "Refuge Test"]);
    git_output(&source, &["config", "user.email", "refuge@example.invalid"]);
    std::fs::write(source.join("entry.txt"), "one\n").unwrap();
    git_output(&source, &["add", "entry.txt"]);
    git_output(&source, &["commit", "-m", "initial"]);
    git_output(&source, &["tag", "-a", "v1", "-m", "version one"]);
    let expected = git::ref_state(&source).unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "import", "notes", source.to_str().unwrap()])
        .assert()
        .success();

    let imported = repos.join("notes.git");
    assert_eq!(git::ref_state(&imported).unwrap().refs, expected.refs);
}

#[test]
fn repo_create_rejects_unsafe_name() {
    let temp = tempfile::tempdir().unwrap();
    let (config, _) = initialized(&temp);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "../escape"])
        .assert()
        .failure()
        .stderr(contains("invalid repository name"));
}
