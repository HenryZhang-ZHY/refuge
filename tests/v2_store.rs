mod support;

use std::path::{Path, PathBuf};

use predicates::str::contains;
use refuge::manifest::Manifest;
use support::TestEnvironment;

fn manifests(target: &Path, repo_id: &str) -> Vec<(PathBuf, Manifest)> {
    let directory = target
        .join("refuge/v2/repos")
        .join(repo_id)
        .join("snapshots");
    let mut values = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .map(|path| {
            let manifest: Manifest =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            (path, manifest)
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|(_, manifest)| manifest.generation);
    values
}

fn working_copy(env: &TestEnvironment, name: &str) -> (PathBuf, PathBuf, String) {
    env.refuge()
        .args(["repo", "create", name])
        .assert()
        .success();
    let hosted = env.repos.join(format!("{name}.git"));
    let id = env.git(&hosted, &["config", "refuge.repoid"], false);
    let work = env.path().join(format!("{name}-work"));
    std::fs::create_dir(&work).unwrap();
    env.git(&work, &["init", "--initial-branch=main"], false);
    env.git(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        false,
    );
    (hosted, work, id)
}

fn commit_and_push(env: &TestEnvironment, work: &Path, value: &str) {
    std::fs::write(work.join("note.txt"), value).unwrap();
    env.git(work, &["add", "note.txt"], false);
    env.git(work, &["commit", "-m", value], false);
    env.git(work, &["push", "refuge", "main"], true);
}

#[test]
fn lifecycle_is_incremental_idempotent_and_checkpointable() {
    let env = TestEnvironment::new();
    env.initialize();
    let (_, work, id) = working_copy(&env, "notes");
    commit_and_push(&env, &work, "one");
    commit_and_push(&env, &work, "two");
    let snapshots = manifests(&env.target, &id);
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots[1].1.git.parent.is_none());
    assert_eq!(
        snapshots[2].1.git.parent.as_deref(),
        Some(snapshots[1].1.snapshot_id.as_str())
    );

    env.refuge()
        .args(["repo", "backup", "notes"])
        .assert()
        .success()
        .stdout(contains("already protected by"));
    assert_eq!(manifests(&env.target, &id).len(), 3);
    env.refuge()
        .args(["repo", "backup", "notes", "--checkpoint"])
        .assert()
        .success()
        .stdout(contains("checkpoint"));
    assert!(
        manifests(&env.target, &id)
            .last()
            .unwrap()
            .1
            .git
            .parent
            .is_none()
    );
}

#[test]
fn refs_only_verify_and_usage_are_exposed_by_the_cli() {
    let env = TestEnvironment::new();
    env.initialize();
    let (hosted, work, id) = working_copy(&env, "vault");
    commit_and_push(&env, &work, "one");
    let tip = env.git(&hosted, &["rev-parse", "refs/heads/main"], false);
    env.git(&hosted, &["update-ref", "refs/heads/existing", &tip], false);
    env.refuge()
        .args(["repo", "backup", "vault"])
        .assert()
        .success()
        .stdout(contains("refs only"));
    let newest = manifests(&env.target, &id).pop().unwrap().1;
    assert!(newest.git.parent.is_some());
    assert!(newest.git.bundle.is_none());

    env.refuge()
        .args(["snapshots", "verify", "vault"])
        .assert()
        .success()
        .stdout(contains("verified"));
    env.refuge()
        .args(["snapshots", "usage", "vault"])
        .assert()
        .success()
        .stdout(contains("checkpoints"))
        .stdout(contains("manifests"));
}

#[test]
fn deep_verify_detects_same_size_bundle_corruption() {
    let env = TestEnvironment::new();
    env.initialize();
    let (_, work, id) = working_copy(&env, "deep");
    commit_and_push(&env, &work, "one");
    let newest = manifests(&env.target, &id).pop().unwrap().1;
    let bundle = newest.git.bundle.unwrap();
    let path = env.target.join("refuge/v2/repos").join(id).join(bundle.key);
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    std::fs::write(path, bytes).unwrap();
    env.refuge()
        .args(["snapshots", "list", "deep"])
        .assert()
        .success()
        .stdout(contains("valid"));
    env.refuge()
        .args(["snapshots", "verify", "deep"])
        .assert()
        .code(1)
        .stdout(contains("checksum differs from manifest"));
}
