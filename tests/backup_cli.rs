mod support;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use predicates::str::contains;
use refuge::manifest::Manifest;
use support::TestEnvironment;

/// PATH value with the built `refuge` binary's directory prepended, so the
/// post-receive hook's `command -v refuge` can find it during tests, the
/// same way it would find a real installation on the user's PATH.
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
    let env = TestEnvironment::new();
    env.initialize();
    env.refuge()
        .args(["repo", "create", "ledger"])
        .assert()
        .success();
    let hosted = env.repos.join("ledger.git");
    let repo_id = env.git(&hosted, &["config", "refuge.repoid"], false);

    let work = env.path().join("work");
    std::fs::create_dir(&work).unwrap();
    env.git(&work, &["init", "--initial-branch=main"], false);
    env.git(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        false,
    );
    std::fs::write(work.join("ledger.bean"), "2026-01-01 open Assets:Cash\n").unwrap();
    env.git(&work, &["add", "ledger.bean"], false);
    env.git(&work, &["commit", "-m", "add ledger"], false);
    env.git(&work, &["push", "refuge", "main"], true);

    let paths = manifests(&env.target, &repo_id);
    assert_eq!(paths.len(), 2);
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&paths[1]).unwrap()).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.repo_name, "ledger");
    assert_eq!(manifest.generation, 2);
    assert!(manifest.refs.contains_key("refs/heads/main"));
    let artifact = manifest.artifact.expect("bundle artifact");
    let bundle = env
        .target
        .join("refuge/v1/repos")
        .join(&repo_id)
        .join(artifact.key);
    assert_eq!(std::fs::metadata(&bundle).unwrap().len(), artifact.size);
    refuge::git::bundle_verify(&hosted, &bundle).unwrap();

    let mut first = env.std_refuge();
    first
        .args(["repo", "backup", "ledger"])
        .stdout(Stdio::null());
    let mut first = first.spawn().unwrap();
    let mut second = env.std_refuge();
    second
        .args(["repo", "backup", "ledger"])
        .stdout(Stdio::null());
    let mut second = second.spawn().unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    let generations: Vec<_> = manifests(&env.target, &repo_id)
        .into_iter()
        .map(|path| {
            serde_json::from_slice::<Manifest>(&std::fs::read(path).unwrap())
                .unwrap()
                .generation
        })
        .collect();
    assert_eq!(generations, [1, 2, 3, 4]);
}

#[test]
fn repository_creation_publishes_empty_manifest_without_artifact() {
    let env = TestEnvironment::new();
    env.initialize();
    env.refuge()
        .args(["repo", "create", "empty"])
        .assert()
        .success();
    let hosted = env.repos.join("empty.git");
    let repo_id = env.git(&hosted, &["config", "refuge.repoid"], false);

    env.refuge()
        .args(["repo", "status", "empty"])
        .assert()
        .success()
        .stdout(contains("Protected"));

    let path = manifests(&env.target, &repo_id).pop().unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest.refs.len(), 1);
    assert_eq!(manifest.head(), Some("refs/heads/main"));
    assert!(manifest.artifact.is_none());

    env.refuge()
        .args(["repo", "status", "empty"])
        .assert()
        .success()
        .stdout(contains("Protected"));

    let blob_path = env.path().join("new-object");
    std::fs::write(&blob_path, "a ref can point to a blob\n").unwrap();
    let oid = env.git(
        &hosted,
        &["hash-object", "-w", blob_path.to_str().unwrap()],
        false,
    );
    env.git(&hosted, &["update-ref", "refs/notes/pending", &oid], false);
    env.refuge()
        .args(["repo", "status", "empty"])
        .assert()
        .success()
        .stdout(contains("Pending"));
}

#[test]
fn snapshot_listing_reports_a_missing_artifact_as_corrupt() {
    let env = TestEnvironment::new();
    env.initialize();
    env.refuge()
        .args(["repo", "create", "documents"])
        .assert()
        .success();
    let hosted = env.repos.join("documents.git");
    let repo_id = env.git(&hosted, &["config", "refuge.repoid"], false);

    let work = env.path().join("work");
    std::fs::create_dir(&work).unwrap();
    env.git(&work, &["init", "--initial-branch=main"], false);
    env.git(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        false,
    );
    std::fs::write(work.join("note.md"), "remember this\n").unwrap();
    env.git(&work, &["add", "note.md"], false);
    env.git(&work, &["commit", "-m", "add note"], false);
    env.git(&work, &["push", "refuge", "main"], true);

    let path = manifests(&env.target, &repo_id).pop().unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let artifact = manifest.artifact.unwrap();
    std::fs::remove_file(
        env.target
            .join("refuge/v1/repos")
            .join(repo_id)
            .join(artifact.key),
    )
    .unwrap();

    env.refuge()
        .args(["snapshots", "list", "documents"])
        .assert()
        .success()
        .stdout(contains("corrupt"));
}

#[test]
fn invalid_manifest_does_not_block_an_unrelated_repository() {
    let env = TestEnvironment::new();
    env.initialize();
    for name in ["healthy", "damaged"] {
        env.refuge()
            .args(["repo", "create", name])
            .assert()
            .success();
    }
    let damaged = env.repos.join("damaged.git");
    let damaged_id = env.git(&damaged, &["config", "refuge.repoid"], false);
    let damaged_manifest = manifests(&env.target, &damaged_id).pop().unwrap();
    std::fs::write(&damaged_manifest, b"not JSON").unwrap();

    env.refuge()
        .args(["snapshots", "list", "healthy"])
        .assert()
        .success()
        .stdout(contains("healthy"))
        .stderr(contains(damaged_manifest.display().to_string()));
}

#[test]
fn explicitly_selected_unsupported_manifest_is_rejected() {
    let env = TestEnvironment::new();
    env.initialize();
    env.refuge()
        .args(["repo", "create", "future"])
        .assert()
        .success();
    let hosted = env.repos.join("future.git");
    let repo_id = env.git(&hosted, &["config", "refuge.repoid"], false);
    let path = manifests(&env.target, &repo_id).pop().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["schema_version"] = 2.into();
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let snapshot_id = path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_suffix(".manifest.json")
        .unwrap();

    env.refuge()
        .args([
            "restore",
            &repo_id,
            "--snapshot",
            snapshot_id,
            "--as",
            "rejected",
        ])
        .assert()
        .failure()
        .stderr(contains("Unsupported"))
        .stderr(contains("schema version 2"));
    assert!(!env.repos.join("rejected.git").exists());
}
