//! Executable step definitions for `tests/features/lfs_backup_restore.feature`.
//!
//! This exercises a real `git-lfs` client end to end: track, commit, push,
//! and pull. It is a black-box check that Refuge's backup/restore also
//! covers a hosted repository's LFS content store, not just its git objects.

use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

use assert_cmd::Command;
use cucumber::{World as _, given, then, when};
use refuge::manifest::Manifest;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct LfsWorld {
    temp: TempDir,
    config: PathBuf,
    repos: PathBuf,
    one_drive: PathBuf,
    target: PathBuf,
    hosted: Option<PathBuf>,
    work: Option<PathBuf>,
    repo_id: Option<String>,
    push_output: Option<Output>,
    binary_content: Vec<u8>,
    clean_config: Option<PathBuf>,
    clean_repos: Option<PathBuf>,
    restored: Option<PathBuf>,
}

impl LfsWorld {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary BDD world");
        let config = temp.path().join("config.toml");
        let repos = temp.path().join("local-repositories");
        let one_drive = temp.path().join("OneDrive - Example Company");
        let target = one_drive.join("Refuge Backups");
        std::fs::create_dir(&one_drive).expect("OneDrive fixture directory");
        Self {
            temp,
            config,
            repos,
            one_drive,
            target,
            hosted: None,
            work: None,
            repo_id: None,
            push_output: None,
            binary_content: Vec::new(),
            clean_config: None,
            clean_repos: None,
            restored: None,
        }
    }

    fn command_for(&self, config: &Path) -> Command {
        let mut command = Command::cargo_bin("refuge").expect("refuge binary");
        command
            .env("REFUGE_CONFIG", config)
            .env("OneDriveCommercial", &self.one_drive);
        command
    }

    fn command(&self) -> Command {
        self.command_for(&self.config)
    }

    fn initialize(&mut self) {
        self.command()
            .args([
                "init",
                "--repos",
                self.repos.to_str().unwrap(),
                "--target",
                self.target.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    fn create_repository(&mut self, name: &str) {
        self.command()
            .args(["repo", "create", name])
            .assert()
            .success();
        let hosted = self.repos.join(format!("{name}.git"));
        self.repo_id = Some(git_stdout(&hosted, &["config", "refuge.repoid"], None));
        self.hosted = Some(hosted);
    }

    fn prepare_working_copy(&mut self, name: &str) {
        self.initialize();
        self.create_repository(name);
        let work = self.temp.path().join("working-copy");
        std::fs::create_dir(&work).expect("working copy directory");
        git_stdout(&work, &["init", "--initial-branch=main"], None);
        git_stdout(&work, &["config", "user.name", "Refuge User"], None);
        git_stdout(
            &work,
            &["config", "user.email", "user@example.invalid"],
            None,
        );
        git_stdout(
            &work,
            &["remote", "add", "refuge", self.hosted().to_str().unwrap()],
            None,
        );
        self.work = Some(work);
    }

    fn hosted(&self) -> &Path {
        self.hosted.as_deref().expect("hosted repository")
    }

    fn work(&self) -> &Path {
        self.work.as_deref().expect("working copy")
    }

    fn repo_id(&self) -> &str {
        self.repo_id.as_deref().expect("repository id")
    }

    fn manifests(&self) -> Vec<Manifest> {
        let directory = self
            .target
            .join("refuge/v1/repos")
            .join(self.repo_id())
            .join("snapshots");
        let mut paths: Vec<_> = std::fs::read_dir(directory)
            .expect("snapshot directory")
            .map(|entry| entry.expect("snapshot entry").path())
            .filter(|path| path.to_string_lossy().ends_with(".manifest.json"))
            .collect();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                serde_json::from_slice(&std::fs::read(path).expect("manifest bytes"))
                    .expect("valid manifest")
            })
            .collect()
    }
}

fn git_output(repo: &Path, args: &[&str], config: Option<&Path>) -> Output {
    let mut command = ProcessCommand::new("git");
    // Newer git defaults to `safe.bareRepository = explicit`, which refuses
    // to auto-detect a bare repository via `-C`. These steps intentionally
    // invoke git this way against bare hosted repos, so opt back in.
    command
        .args(["-c", "safe.bareRepository=all"])
        .arg("-C")
        .arg(repo)
        .args(args);
    if let Some(config) = config {
        command.env("REFUGE_CONFIG", config);
    }
    command.output().expect("run git")
}

fn git_stdout(repo: &Path, args: &[&str], config: Option<&Path>) -> String {
    let output = git_output(repo, args, config);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

#[given(expr = "the {string} working copy uses the Refuge repository as a remote")]
fn working_copy_uses_refuge(world: &mut LfsWorld, name: String) {
    world.prepare_working_copy(&name);
}

#[given(expr = "the working copy tracks {string} files with Git LFS")]
fn tracks_pattern_with_lfs(world: &mut LfsWorld, pattern: String) {
    let work = world.work().to_path_buf();
    git_stdout(&work, &["lfs", "install", "--local"], None);
    git_stdout(&work, &["lfs", "track", &pattern], None);
    git_stdout(&work, &["add", ".gitattributes"], None);
    git_stdout(&work, &["commit", "-m", "track large files with Git LFS"], None);
}

#[when("the user commits a large binary file and pushes the main branch")]
fn commit_large_binary_and_push(world: &mut LfsWorld) {
    let work = world.work().to_path_buf();
    // Deterministic, non-trivial content so a later byte-for-byte
    // comparison after restore is meaningful.
    let content: Vec<u8> = (0..2_000_000).map(|index| (index % 251) as u8).collect();
    std::fs::write(work.join("asset.bin"), &content).expect("write binary asset");
    world.binary_content = content;
    git_stdout(&work, &["add", "asset.bin"], None);
    git_stdout(&work, &["commit", "-m", "add large binary asset"], None);
    world.push_output = Some(git_output(
        &work,
        &["push", "refuge", "main"],
        Some(&world.config),
    ));
}

#[then("the push succeeds without a separate backup command")]
fn push_succeeds(world: &mut LfsWorld) {
    let output = world.push_output.as_ref().expect("a push was attempted");
    assert!(
        output.status.success(),
        "git push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[then("the push output reports LFS bytes protected locally")]
fn push_output_reports_lfs_bytes(world: &mut LfsWorld) {
    let output = world.push_output.as_ref().expect("a push was attempted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("protected"));
    assert!(stderr.contains("LFS bytes"), "push stderr: {stderr}");
}

#[then("status says the repository is protected locally")]
fn status_says_protected_locally(world: &mut LfsWorld) {
    let status = world
        .command()
        .args(["status", "vault"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8(status).expect("UTF-8 status");
    assert!(status.contains("Protected locally"));
}

#[then("a verified LFS archive appears in the OneDrive sync folder")]
fn verified_lfs_archive_appears(world: &mut LfsWorld) {
    let manifests = world.manifests();
    let manifest = manifests.last().expect("at least one snapshot");
    let artifact = manifest
        .lfs_artifact
        .as_ref()
        .expect("manifest records an LFS artifact");
    let archive = world
        .target
        .join("refuge/v1/repos")
        .join(world.repo_id())
        .join(&artifact.key);
    let bytes = std::fs::read(&archive).expect("published LFS archive readable");
    assert_eq!(bytes.len() as u64, artifact.size);
    let mut digest = Sha256::new();
    digest.update(&bytes);
    let checksum = format!("sha256:{:x}", digest.finalize());
    assert_eq!(checksum, artifact.checksum);
}

#[when(expr = "a clean Refuge installation restores the {string} repository")]
fn clean_installation_restores(world: &mut LfsWorld, name: String) {
    let clean_config = world.temp.path().join("clean-config.toml");
    let clean_repos = world.temp.path().join("clean-repositories");
    world
        .command_for(&clean_config)
        .args([
            "init",
            "--repos",
            clean_repos.to_str().unwrap(),
            "--target",
            world.target.to_str().unwrap(),
        ])
        .assert()
        .success();
    world
        .command_for(&clean_config)
        .args(["restore", &name])
        .assert()
        .success();
    world.clean_config = Some(clean_config);
    world.restored = Some(clean_repos.join(format!("{name}.git")));
    world.clean_repos = Some(clean_repos);
}

#[then("the restored repository's Git LFS objects match the original content")]
fn restored_lfs_objects_match(world: &mut LfsWorld) {
    let restored = world.restored.as_deref().expect("restored repository");
    let clone = world.temp.path().join("restored-working-copy");
    let clean_config = world.clean_config.as_ref().expect("clean config");
    git_stdout(
        world.temp.path(),
        &[
            "clone",
            restored.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
        None,
    );
    git_stdout(&clone, &["lfs", "pull"], Some(clean_config));
    let restored_content = std::fs::read(clone.join("asset.bin")).expect("restored asset");
    assert_eq!(restored_content, world.binary_content);
}

#[tokio::main]
async fn main() {
    LfsWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/lfs_backup_restore.feature")
        .await;
}
