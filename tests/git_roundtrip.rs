use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use refuge::git::{self, RefState};
use tempfile::TempDir;

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        // Newer git defaults to `safe.bareRepository = explicit`, which
        // refuses to auto-detect a bare repository via `-C`. This helper is
        // used against bare repos elsewhere in this file, so opt back in.
        .args(["-c", "safe.bareRepository=all"])
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
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "safe.bareRepository=all"])
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture() -> TempDir {
    let temp = tempfile::tempdir().expect("temp dir");
    run_git(temp.path(), &["init", "--initial-branch=main"]);
    run_git(temp.path(), &["config", "user.name", "Refuge Test"]);
    run_git(
        temp.path(),
        &["config", "user.email", "refuge@example.invalid"],
    );
    std::fs::write(temp.path().join("entry.txt"), "protected\n").expect("fixture file");
    run_git(temp.path(), &["add", "entry.txt"]);
    run_git(temp.path(), &["commit", "-m", "initial"]);
    run_git(temp.path(), &["tag", "-a", "v1", "-m", "version one"]);
    run_git(
        temp.path(),
        &["notes", "--ref=refs/notes/review", "add", "-m", "reviewed"],
    );
    temp
}

#[test]
fn ref_state_hash_is_canonical() {
    let first = RefState {
        refs: BTreeMap::from([
            ("refs/tags/v1".into(), "2222".into()),
            ("refs/heads/main".into(), "1111".into()),
        ]),
        head: Some("refs/heads/main".into()),
    };
    let second = RefState {
        refs: BTreeMap::from([
            ("refs/heads/main".into(), "1111".into()),
            ("refs/tags/v1".into(), "2222".into()),
        ]),
        head: Some("refs/heads/main".into()),
    };

    assert_eq!(first.hash(), second.hash());
    assert!(first.hash().starts_with("sha256:"));
}

#[test]
fn bundle_round_trips_all_ref_types() {
    let source = fixture();
    let state = git::ref_state(source.path()).expect("read ref state");
    assert_eq!(state.head.as_deref(), Some("refs/heads/main"));
    assert!(state.refs.contains_key("refs/heads/main"));
    assert!(state.refs.contains_key("refs/tags/v1"));
    assert!(state.refs.contains_key("refs/notes/review"));

    let bundle = source.path().join("fixture.bundle");
    git::bundle_create(source.path(), &bundle, &[]).expect("create bundle");
    git::bundle_verify(source.path(), &bundle).expect("verify bundle");
    assert_eq!(git::bundle_list_heads(&bundle).unwrap(), state.refs);

    let restored = source.path().join("restored.git");
    git::clone_mirror(&bundle, &restored).expect("clone mirror");
    git::set_symbolic_head(&restored, state.head.as_deref().unwrap()).expect("restore HEAD");
    assert_eq!(git::ref_state(&restored).unwrap(), state);
}

#[test]
fn empty_repository_has_head_but_no_refs() {
    let temp = tempfile::tempdir().expect("temp dir");
    run_git(temp.path(), &["init", "--bare", "--initial-branch=main"]);

    let state = git::ref_state(temp.path()).expect("read empty ref state");

    assert!(state.refs.is_empty());
    assert_eq!(state.head.as_deref(), Some("refs/heads/main"));
}

#[test]
fn batch_queries_peel_commits_and_reject_non_blobs() {
    let source = fixture();
    let commit = git_output(source.path(), &["rev-parse", "HEAD"]);
    let tag = git_output(source.path(), &["rev-parse", "refs/tags/v1"]);
    let blob = git_output(source.path(), &["rev-parse", "HEAD:entry.txt"]);
    let missing = "0".repeat(40);
    let names = vec![
        commit.clone(),
        format!("{tag}^{{commit}}"),
        blob.clone(),
        format!("{blob}^{{commit}}"),
        missing,
    ];
    let checks = git::batch_check(source.path(), &names).unwrap();
    assert!(matches!(&checks[0], git::BatchCheck::Found { kind, .. } if kind == "commit"));
    assert!(matches!(&checks[1], git::BatchCheck::Found { kind, .. } if kind == "commit"));
    assert!(matches!(&checks[2], git::BatchCheck::Found { kind, .. } if kind == "blob"));
    assert!(matches!(checks[3], git::BatchCheck::Missing));
    assert!(matches!(checks[4], git::BatchCheck::Missing));
    assert_eq!(
        git::batch_blob_contents(source.path(), &[blob]).unwrap()[0].1,
        b"protected\n"
    );
}

#[test]
fn incremental_bundle_requires_its_base_and_replays_exact_refs() {
    let source = tempfile::tempdir().unwrap();
    run_git(source.path(), &["init", "--initial-branch=main"]);
    run_git(source.path(), &["config", "user.name", "Refuge Test"]);
    run_git(
        source.path(),
        &["config", "user.email", "refuge@example.invalid"],
    );
    std::fs::write(source.path().join("file"), "a").unwrap();
    run_git(source.path(), &["add", "file"]);
    run_git(source.path(), &["commit", "-m", "A"]);
    let a = git_output(source.path(), &["rev-parse", "HEAD"]);
    let full = source.path().join("full.bundle");
    git::bundle_create(source.path(), &full, &[]).unwrap();
    std::fs::write(source.path().join("file"), "b").unwrap();
    run_git(source.path(), &["commit", "-am", "B"]);
    let delta = source.path().join("delta.bundle");
    git::bundle_create(source.path(), &delta, std::slice::from_ref(&a)).unwrap();
    assert_eq!(git::bundle_list_heads(&delta).unwrap().len(), 1);

    let empty = source.path().join("empty.git");
    git::init_bare(&empty).unwrap();
    assert!(git::bundle_unbundle(&empty, &delta).is_err());
    git::bundle_unbundle(&empty, &full).unwrap();
    git::bundle_unbundle(&empty, &delta).unwrap();
    let state = git::ref_state(source.path()).unwrap();
    git::create_refs(&empty, &state.refs).unwrap();
    git::set_symbolic_head(&empty, state.head.as_deref().unwrap()).unwrap();
    assert_eq!(git::ref_state(&empty).unwrap(), state);
    git::fsck(&empty).unwrap();
}

#[test]
fn ref_creation_is_atomic_and_exclusion_bound_is_supported() {
    let source = fixture();
    let oid = git_output(source.path(), &["rev-parse", "HEAD"]);
    let tips = git::ref_state(source.path())
        .unwrap()
        .refs
        .into_values()
        .collect::<Vec<_>>();
    let exclusions = (0..git::MAX_EXCLUSIONS)
        .map(|index| tips[index % tips.len()].clone())
        .collect::<Vec<_>>();
    assert!(!git::has_objects_outside(source.path(), &exclusions).unwrap());
    let target = source.path().join("target.git");
    git::init_bare(&target).unwrap();
    let bundle = source.path().join("objects.bundle");
    git::bundle_create(source.path(), &bundle, &[]).unwrap();
    git::bundle_unbundle(&target, &bundle).unwrap();
    let refs = BTreeMap::from([
        ("refs/heads/good".into(), oid),
        ("refs/heads/bad".into(), "0".repeat(40)),
    ]);
    assert!(git::create_refs(&target, &refs).is_err());
    assert!(git::ref_state(&target).unwrap().refs.is_empty());
}
