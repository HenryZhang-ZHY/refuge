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
    git::bundle_create(source.path(), &bundle).expect("create bundle");
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
