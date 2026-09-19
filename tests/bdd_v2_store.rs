//! Executable behavior specification for the v2 snapshot store.

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use cucumber::{World as _, given, then, when};
use refuge::manifest::Manifest;
use support::TestEnvironment;

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct V2World {
    env: TestEnvironment,
    hosted: Option<PathBuf>,
    work: Option<PathBuf>,
    repo_id: Option<String>,
    commit_number: u32,
    manifest_count: usize,
    output: Option<Output>,
}

impl V2World {
    fn new() -> Self {
        Self {
            env: TestEnvironment::new(),
            hosted: None,
            work: None,
            repo_id: None,
            commit_number: 0,
            manifest_count: 0,
            output: None,
        }
    }

    fn setup(&mut self, name: &str) {
        self.env.initialize();
        self.env
            .refuge()
            .args(["repo", "create", name])
            .assert()
            .success();
        let hosted = self.env.repos.join(format!("{name}.git"));
        let repo_id = self.env.git(&hosted, &["config", "refuge.repoid"], false);
        let work = self.env.path().join("working-copy");
        std::fs::create_dir(&work).unwrap();
        self.env
            .git(&work, &["init", "--initial-branch=main"], false);
        self.env.git(
            &work,
            &["remote", "add", "refuge", hosted.to_str().unwrap()],
            false,
        );
        self.hosted = Some(hosted);
        self.work = Some(work);
        self.repo_id = Some(repo_id);
        self.push_commit();
    }

    fn hosted(&self) -> &Path {
        self.hosted.as_deref().expect("hosted repository")
    }

    fn work(&self) -> &Path {
        self.work.as_deref().expect("working copy")
    }

    fn root(&self) -> PathBuf {
        self.env
            .target
            .join("refuge/v2/repos")
            .join(self.repo_id.as_deref().expect("repository id"))
    }

    fn manifests(&self) -> Vec<Manifest> {
        let mut values = std::fs::read_dir(self.root().join("snapshots"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
            .collect::<Vec<Manifest>>();
        values.sort_by_key(|manifest| manifest.generation);
        values
    }

    fn push_commit(&mut self) {
        self.commit_number += 1;
        let work = self.work().to_path_buf();
        std::fs::write(
            work.join("note.txt"),
            format!("version {}\n", self.commit_number),
        )
        .unwrap();
        self.env.git(&work, &["add", "note.txt"], false);
        self.env.git(
            &work,
            &["commit", "-m", &format!("version {}", self.commit_number)],
            false,
        );
        let output = self
            .env
            .git_output(&work, &["push", "refuge", "main"], true);
        assert!(
            output.status.success(),
            "push failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.output = Some(output);
    }

    fn command_output(&mut self, args: &[&str]) {
        self.output = Some(self.env.refuge().args(args).output().unwrap());
    }

    fn output(&self) -> &Output {
        self.output.as_ref().expect("command output")
    }
}

#[given(expr = "a v2 repository named {string} has one protected commit")]
fn repository_has_one_commit(world: &mut V2World, name: String) {
    world.setup(&name);
}

#[when("the user pushes a second commit")]
fn pushes_second_commit(world: &mut V2World) {
    world.push_commit();
}

#[then("the published recovery points are empty, checkpoint, and delta")]
fn recovery_points_have_expected_kinds(world: &mut V2World) {
    let manifests = world.manifests();
    assert_eq!(manifests.len(), 3);
    assert_eq!(manifests[0].object_ref_count(), 0);
    assert!(manifests[1].git.parent.is_none());
    assert!(manifests[1].git.bundle.is_some());
    assert_eq!(
        manifests[2].git.parent.as_deref(),
        Some(manifests[1].snapshot_id.as_str())
    );
    assert!(manifests[2].git.bundle.is_some());
}

#[then("the repository remains protected")]
fn repository_remains_protected(world: &mut V2World) {
    world
        .env
        .refuge()
        .args(["repo", "status", "notes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Protected locally"));
}

#[when("the user backs up unchanged refs")]
fn backs_up_unchanged_refs(world: &mut V2World) {
    world.manifest_count = world.manifests().len();
    world.command_output(&["repo", "backup", "notes"]);
}

#[then("Refuge reports that the repository is already protected")]
fn reports_already_protected(world: &mut V2World) {
    assert!(world.output().status.success());
    assert!(String::from_utf8_lossy(&world.output().stdout).contains("already protected by"));
}

#[then("no new manifest is published")]
fn no_new_manifest(world: &mut V2World) {
    assert_eq!(world.manifests().len(), world.manifest_count);
}

#[when("the user forces a checkpoint backup")]
fn forces_checkpoint(world: &mut V2World) {
    world.command_output(&["repo", "backup", "notes", "--checkpoint"]);
}

#[then("a new checkpoint manifest is published")]
fn checkpoint_is_published(world: &mut V2World) {
    assert!(world.output().status.success());
    assert!(String::from_utf8_lossy(&world.output().stdout).contains("checkpoint"));
    let newest = world.manifests().pop().unwrap();
    assert!(newest.git.parent.is_none());
    assert!(newest.git.bundle.is_some());
}

#[when("the user adds a branch at an existing commit and backs up")]
fn adds_existing_branch(world: &mut V2World) {
    let tip = world
        .env
        .git(world.hosted(), &["rev-parse", "refs/heads/main"], false);
    let hosted = world.hosted().to_path_buf();
    world
        .env
        .git(&hosted, &["update-ref", "refs/heads/existing", &tip], false);
    world.command_output(&["repo", "backup", "notes"]);
}

#[then("the newest recovery point is refs-only")]
fn newest_is_refs_only(world: &mut V2World) {
    assert!(world.output().status.success());
    assert!(String::from_utf8_lossy(&world.output().stdout).contains("refs only"));
    let newest = world.manifests().pop().unwrap();
    assert!(newest.git.parent.is_some());
    assert!(newest.git.bundle.is_none());
}

#[then("deep verification succeeds")]
fn verification_succeeds(world: &mut V2World) {
    world
        .env
        .refuge()
        .args(["snapshots", "verify", "notes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verified"));
}

#[given("the newest bundle is corrupted without changing its size")]
fn corrupts_bundle_same_size(world: &mut V2World) {
    let newest = world.manifests().pop().unwrap();
    let bundle = newest.git.bundle.unwrap();
    let path = world.root().join(bundle.key);
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    std::fs::write(path, bytes).unwrap();
}

#[when("the user deeply verifies the repository")]
fn deeply_verifies(world: &mut V2World) {
    world.command_output(&["snapshots", "verify", "notes"]);
}

#[then("verification reports invalid with exit code 1")]
fn verification_is_invalid(world: &mut V2World) {
    assert_eq!(world.output().status.code(), Some(1));
    assert!(String::from_utf8_lossy(&world.output().stdout).contains("invalid"));
}

#[then("shallow snapshot listing still reports valid")]
fn shallow_listing_is_valid(world: &mut V2World) {
    world
        .env
        .refuge()
        .args(["snapshots", "list", "notes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("valid"));
}

#[when("the user asks for snapshot usage")]
fn asks_for_usage(world: &mut V2World) {
    world.command_output(&["snapshots", "usage", "notes"]);
}

#[then("usage reports checkpoints, LFS objects, manifests, and total bytes")]
fn usage_has_categories(world: &mut V2World) {
    assert!(world.output().status.success());
    let stdout = String::from_utf8_lossy(&world.output().stdout);
    for category in ["checkpoints", "lfs objects", "manifests", "total"] {
        assert!(stdout.contains(category), "usage output: {stdout}");
    }
}

#[given("a conflicting file appears in the snapshots directory")]
fn creates_conflict_copy(world: &mut V2World) {
    std::fs::write(world.root().join("snapshots/conflict-copy.json"), b"{}\n").unwrap();
}

#[when("the user checks repository status")]
fn checks_status(world: &mut V2World) {
    world.command_output(&["repo", "status", "notes"]);
}

#[then("status reports corruption and names the conflicting file")]
fn status_names_conflict(world: &mut V2World) {
    assert!(world.output().status.success());
    let stdout = String::from_utf8_lossy(&world.output().stdout);
    assert!(stdout.contains("corrupt"));
    assert!(stdout.contains("conflict-copy.json"));
}

#[given("the newest delta bundle is truncated")]
fn truncates_newest_delta(world: &mut V2World) {
    world.push_commit();
    let newest = world.manifests().pop().unwrap();
    assert!(newest.git.parent.is_some());
    let bundle = newest.git.bundle.unwrap();
    let path = world.root().join(bundle.key);
    let length = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(length / 2)
        .unwrap();
}

#[when("the user pushes another commit")]
fn pushes_another_commit(world: &mut V2World) {
    world.push_commit();
}

#[then("Refuge publishes a checkpoint and protection is restored")]
fn publishes_healing_checkpoint(world: &mut V2World) {
    let stderr = String::from_utf8_lossy(&world.output().stderr);
    assert!(stderr.contains("checkpoint"), "push output: {stderr}");
    let newest = world.manifests().pop().unwrap();
    assert!(newest.git.parent.is_none());
    repository_remains_protected(world);
}

#[then("the target contains no locks, staging directories, or partial files")]
fn target_is_clean(world: &mut V2World) {
    fn visit(path: &Path, invalid: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".lock") || name.ends_with(".tmp") || name.starts_with(".refuge-") {
                invalid.push(path.clone());
            }
            if path.is_dir() {
                visit(&path, invalid);
            }
        }
    }
    let mut invalid = Vec::new();
    visit(&world.env.target, &mut invalid);
    assert!(invalid.is_empty(), "temporary target entries: {invalid:?}");
}

#[tokio::main]
async fn main() {
    V2World::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/v2_snapshot_store.feature")
        .await;
}
