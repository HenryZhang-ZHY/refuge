//! Executable counterparts to `tests/features/first_use.feature`.
//!
//! Each test maps one-to-one to the scenario carrying the same `@Sxx` tag.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use refuge::config::Config;
use refuge::git;
use refuge::manifest::Manifest;
use tempfile::TempDir;

struct World {
    _temp: TempDir,
    config: PathBuf,
    repos: PathBuf,
    one_drive: PathBuf,
    target: PathBuf,
}

impl World {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let repos = temp.path().join("local-repositories");
        let one_drive = temp.path().join("OneDrive - Example Company");
        let target = one_drive.join("Refuge Backups");
        std::fs::create_dir(&one_drive).unwrap();
        Self {
            _temp: temp,
            config,
            repos,
            one_drive,
            target,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("refuge").unwrap();
        command
            .env("REFUGE_CONFIG", &self.config)
            .env("OneDriveCommercial", &self.one_drive);
        command
    }

    fn initialize(&self) -> String {
        let output = self
            .command()
            .args([
                "init",
                "--repos",
                self.repos.to_str().unwrap(),
                "--target",
                self.target.to_str().unwrap(),
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    }

    fn create_repository(&self, name: &str) -> String {
        let output = self
            .command()
            .args(["repo", "create", name])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    }

    fn hosted(&self, name: &str) -> PathBuf {
        self.repos.join(format!("{name}.git"))
    }

    fn manifests(&self, repo_id: &str) -> Vec<Manifest> {
        let directory = self
            .target
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
            .into_iter()
            .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
            .collect()
    }
}

struct ConnectedRepository {
    world: World,
    work: PathBuf,
    hosted: PathBuf,
    repo_id: String,
}

fn first_push() -> ConnectedRepository {
    let world = World::new();
    world.initialize();
    world.create_repository("notes");
    let hosted = world.hosted("notes");
    let repo_id = git_stdout(&hosted, &["config", "refuge.repoid"], None);
    let work = world._temp.path().join("working-copy");
    std::fs::create_dir(&work).unwrap();
    git_stdout(&work, &["init", "--initial-branch=main"], None);
    git_stdout(&work, &["config", "user.name", "Refuge User"], None);
    git_stdout(
        &work,
        &["config", "user.email", "user@example.invalid"],
        None,
    );
    std::fs::write(work.join("notes.md"), "first note\n").unwrap();
    git_stdout(&work, &["add", "notes.md"], None);
    git_stdout(&work, &["commit", "-m", "add first note"], None);
    git_stdout(
        &work,
        &["remote", "add", "refuge", hosted.to_str().unwrap()],
        None,
    );
    git_stdout(&work, &["push", "refuge", "main"], Some(&world.config));
    ConnectedRepository {
        world,
        work,
        hosted,
        repo_id,
    }
}

fn git_stdout(repo: &Path, args: &[&str], config: Option<&Path>) -> String {
    let mut command = ProcessCommand::new("git");
    command.arg("-C").arg(repo).args(args);
    if let Some(config) = config {
        command.env("REFUGE_CONFIG", config);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

// @S01 Configure local repositories and a OneDrive backup target.
#[test]
fn s01_configure_local_repositories_and_onedrive_target() {
    let world = World::new();
    let output = world.initialize();
    let config = Config::load_from(&world.config).unwrap();

    assert_eq!(config.repos_dir, world.repos.canonicalize().unwrap());
    assert_eq!(config.target_root, world.target.canonicalize().unwrap());
    assert!(output.contains(&world.repos.display().to_string()));
    assert!(output.contains(&world.target.display().to_string()));
    assert!(output.contains("Cloud upload is not verified"));
    assert!(output.contains("refuge repo create <name>"));
}

// @S02 Create a repository and connect an existing working copy.
#[test]
fn s02_create_repository_and_print_remote_command() {
    let world = World::new();
    world.initialize();
    let output = world.create_repository("notes");
    let hosted = world.hosted("notes");

    assert!(hosted.is_dir());
    assert!(!hosted.starts_with(&world.one_drive));
    assert_eq!(
        git_stdout(&hosted, &["rev-parse", "--is-bare-repository"], None),
        "true"
    );
    assert!(output.contains(&format!("git remote add refuge \"{}\"", hosted.display())));
}

// @S03 A push automatically publishes a verified local snapshot.
#[test]
fn s03_push_automatically_publishes_verified_snapshot() {
    let repository = first_push();
    let manifests = repository.world.manifests(&repository.repo_id);
    assert_eq!(manifests.len(), 1);
    let manifest = &manifests[0];
    let artifact = manifest.artifact.as_ref().unwrap();
    let bundle = repository
        .world
        .target
        .join("refuge/v1/repos")
        .join(&repository.repo_id)
        .join(&artifact.key);
    assert_eq!(std::fs::metadata(&bundle).unwrap().len(), artifact.size);
    git::bundle_verify(&repository.hosted, &bundle).unwrap();

    let status = repository
        .world
        .command()
        .args(["status", "notes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8(status).unwrap();
    assert!(status.contains("Protected locally"));
    assert!(status.contains("Cloud upload is not verified"));
}

// @S04 A later push publishes a new generation without deleting the old one.
#[test]
fn s04_later_push_preserves_old_generation() {
    let repository = first_push();
    std::fs::write(
        repository.work.join("notes.md"),
        "first note\nsecond note\n",
    )
    .unwrap();
    git_stdout(&repository.work, &["add", "notes.md"], None);
    git_stdout(&repository.work, &["commit", "-m", "add second note"], None);
    git_stdout(
        &repository.work,
        &["push", "refuge", "main"],
        Some(&repository.world.config),
    );

    let manifests = repository.world.manifests(&repository.repo_id);
    assert_eq!(
        manifests
            .iter()
            .map(|manifest| manifest.generation)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        manifests.last().unwrap().ref_state_hash,
        git::ref_state(&repository.hosted).unwrap().hash()
    );
}

// @S05 Restore from the OneDrive folder on a clean Refuge installation.
#[test]
fn s05_clean_installation_restores_a_cloneable_remote() {
    let repository = first_push();
    let expected = git::ref_state(&repository.hosted).unwrap();
    let clean_config = repository.world._temp.path().join("clean-config.toml");
    let clean_repos = repository.world._temp.path().join("clean-repositories");

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &clean_config)
        .env("OneDriveCommercial", &repository.world.one_drive)
        .args([
            "init",
            "--repos",
            clean_repos.to_str().unwrap(),
            "--target",
            repository.world.target.to_str().unwrap(),
        ])
        .assert()
        .success();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &clean_config)
        .args(["restore", "notes"])
        .assert()
        .success();

    let restored = clean_repos.join("notes.git");
    assert_eq!(
        git_stdout(&restored, &["config", "refuge.repoid"], None),
        repository.repo_id
    );
    assert_eq!(git::ref_state(&restored).unwrap(), expected);
    let clone = repository.world._temp.path().join("restored-working-copy");
    git_stdout(
        repository.world._temp.path(),
        &["clone", restored.to_str().unwrap(), clone.to_str().unwrap()],
        None,
    );
    assert_eq!(
        std::fs::read_to_string(clone.join("notes.md")).unwrap(),
        "first note\n"
    );
}
