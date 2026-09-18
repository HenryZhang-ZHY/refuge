//! Executable step definitions for `tests/features/first_use.feature`.
//!
//! Cucumber reads the feature file directly. `fail_on_skipped()` makes an
//! edited, undefined, or ambiguous step fail the test process.

use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

use assert_cmd::Command;
use cucumber::{World as _, given, then, when};
use refuge::config::Config;
use refuge::git::{self, RefState};
use refuge::manifest::Manifest;
use tempfile::TempDir;

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct RefugeWorld {
    temp: TempDir,
    config: PathBuf,
    repos: PathBuf,
    one_drive: PathBuf,
    target: PathBuf,
    output: String,
    hosted: Option<PathBuf>,
    work: Option<PathBuf>,
    repo_id: Option<String>,
    push_output: Option<Output>,
    expected_state: Option<RefState>,
    clean_config: Option<PathBuf>,
    clean_repos: Option<PathBuf>,
    restored: Option<PathBuf>,
}

impl RefugeWorld {
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
            output: String::new(),
            hosted: None,
            work: None,
            repo_id: None,
            push_output: None,
            expected_state: None,
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
        self.output = String::from_utf8(output).expect("UTF-8 CLI output");
    }

    fn create_repository(&mut self, name: &str) {
        let output = self
            .command()
            .args(["repo", "create", name])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        self.output = String::from_utf8(output).expect("UTF-8 CLI output");
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

    fn commit_and_push(&mut self, contents: &str, message: &str) {
        let work = self.work().to_path_buf();
        std::fs::write(work.join("notes.md"), contents).expect("write note");
        git_stdout(&work, &["add", "notes.md"], None);
        git_stdout(&work, &["commit", "-m", message], None);
        self.push_output = Some(git_output(
            &work,
            &["push", "refuge", "main"],
            Some(&self.config),
        ));
    }

    fn first_push(&mut self) {
        self.prepare_working_copy("notes");
        self.commit_and_push("first note\n", "add first note");
        self.assert_push_succeeded();
    }

    fn assert_push_succeeded(&self) {
        let output = self.push_output.as_ref().expect("a push was attempted");
        assert!(
            output.status.success(),
            "git push failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
    command.arg("-C").arg(repo).args(args);
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

#[given("a first-time user has a company OneDrive sync folder")]
fn first_time_user(world: &mut RefugeWorld) {
    assert!(world.one_drive.is_dir());
    assert!(!world.config.exists());
}

#[when("they initialize Refuge with a separate local repository directory")]
fn initialize_refuge(world: &mut RefugeWorld) {
    world.initialize();
}

#[then("Refuge stores both resolved paths in its configuration")]
fn stores_resolved_paths(world: &mut RefugeWorld) {
    let config = Config::load_from(&world.config).expect("load Refuge config");
    assert_eq!(config.repos_dir, world.repos.canonicalize().unwrap());
    assert_eq!(config.target_root, world.target.canonicalize().unwrap());
}

#[then("Refuge explains that cloud upload is not verified")]
fn explains_cloud_boundary(world: &mut RefugeWorld) {
    assert!(world.output.contains("Cloud upload is not verified"));
    assert!(world.output.contains(&world.repos.display().to_string()));
    assert!(world.output.contains(&world.target.display().to_string()));
}

#[then("Refuge shows the next command needed to create a repository")]
fn shows_next_command(world: &mut RefugeWorld) {
    assert!(world.output.contains("refuge repo create <name>"));
}

#[given("Refuge has been initialized")]
fn refuge_is_initialized(world: &mut RefugeWorld) {
    world.initialize();
}

#[when(expr = "the user creates a hosted repository named {string}")]
fn create_named_repository(world: &mut RefugeWorld, name: String) {
    world.create_repository(&name);
}

#[then("Refuge creates a bare repository outside OneDrive")]
fn creates_bare_repo_outside_onedrive(world: &mut RefugeWorld) {
    assert!(world.hosted().is_dir());
    assert!(!world.hosted().starts_with(&world.one_drive));
    assert_eq!(
        git_stdout(world.hosted(), &["rev-parse", "--is-bare-repository"], None),
        "true"
    );
}

#[then(expr = "Refuge prints a copyable {string} command")]
fn prints_remote_command(world: &mut RefugeWorld, command: String) {
    assert_eq!(command, "git remote add refuge");
    assert!(world.output.contains(&format!(
        "git remote add refuge \"{}\"",
        world.hosted().display()
    )));
}

#[given(expr = "the {string} working copy uses the Refuge repository as a remote")]
fn working_copy_uses_refuge(world: &mut RefugeWorld, name: String) {
    world.prepare_working_copy(&name);
}

#[when("the user commits a change and pushes the main branch")]
fn commit_and_push_main(world: &mut RefugeWorld) {
    world.commit_and_push("first note\n", "add first note");
}

#[then("the push succeeds without a separate backup command")]
fn push_succeeds(world: &mut RefugeWorld) {
    world.assert_push_succeeded();
}

#[then("a verified bundle and manifest appear in the OneDrive sync folder")]
fn verified_snapshot_appears(world: &mut RefugeWorld) {
    let manifests = world.manifests();
    assert_eq!(manifests.len(), 1);
    let artifact = manifests[0].artifact.as_ref().expect("bundle artifact");
    let bundle = world
        .target
        .join("refuge/v1/repos")
        .join(world.repo_id())
        .join(&artifact.key);
    assert_eq!(std::fs::metadata(&bundle).unwrap().len(), artifact.size);
    git::bundle_verify(world.hosted(), &bundle).expect("verified bundle");
}

#[then("status says the repository is protected locally without claiming cloud confirmation")]
fn status_is_honest(world: &mut RefugeWorld) {
    let status = world
        .command()
        .args(["status", "notes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8(status).expect("UTF-8 status");
    assert!(status.contains("Protected locally"));
    assert!(status.contains("Cloud upload is not verified"));
}

#[given(expr = "the {string} repository has one protected snapshot")]
fn repository_has_snapshot(world: &mut RefugeWorld, name: String) {
    assert_eq!(name, "notes");
    world.first_push();
    assert_eq!(world.manifests().len(), 1);
}

#[when("the user commits and pushes another change")]
fn push_another_change(world: &mut RefugeWorld) {
    world.commit_and_push("first note\nsecond note\n", "add second note");
    world.assert_push_succeeded();
}

#[then("Refuge publishes generation 2 automatically")]
fn publishes_generation_two(world: &mut RefugeWorld) {
    assert_eq!(world.manifests().last().unwrap().generation, 2);
}

#[then("generation 1 remains available")]
fn generation_one_remains(world: &mut RefugeWorld) {
    assert_eq!(
        world
            .manifests()
            .iter()
            .map(|manifest| manifest.generation)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[then("the newest manifest covers the repository's current refs")]
fn newest_manifest_covers_refs(world: &mut RefugeWorld) {
    assert_eq!(
        world.manifests().last().unwrap().ref_state_hash,
        git::ref_state(world.hosted()).unwrap().hash()
    );
}

#[given("the original Refuge repository directory is unavailable")]
fn original_repository_is_unavailable(world: &mut RefugeWorld) {
    world.first_push();
    world.expected_state = Some(git::ref_state(world.hosted()).unwrap());
    std::fs::remove_dir_all(world.hosted()).expect("remove original repository");
}

#[given("a clean Refuge installation points at the existing OneDrive target")]
fn clean_installation_uses_target(world: &mut RefugeWorld) {
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
    world.clean_config = Some(clean_config);
    world.clean_repos = Some(clean_repos);
}

#[when(expr = "the user restores the {string} repository")]
fn restore_repository(world: &mut RefugeWorld, name: String) {
    let config = world.clean_config.as_ref().expect("clean config");
    world
        .command_for(config)
        .args(["restore", &name])
        .assert()
        .success();
    world.restored = Some(
        world
            .clean_repos
            .as_ref()
            .expect("clean repository directory")
            .join(format!("{name}.git")),
    );
}

#[then("the repository identity and refs match the published snapshot")]
fn restored_identity_and_refs_match(world: &mut RefugeWorld) {
    let restored = world.restored.as_deref().expect("restored repository");
    assert_eq!(
        git_stdout(restored, &["config", "refuge.repoid"], None),
        world.repo_id()
    );
    assert_eq!(
        git::ref_state(restored).unwrap(),
        *world.expected_state.as_ref().expect("published ref state")
    );
}

#[then("the restored repository can be cloned as a normal Git remote")]
fn restored_repository_is_cloneable(world: &mut RefugeWorld) {
    let clone = world.temp.path().join("restored-working-copy");
    git_stdout(
        world.temp.path(),
        &[
            "clone",
            world.restored.as_ref().unwrap().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        std::fs::read_to_string(clone.join("notes.md")).unwrap(),
        "first note\n"
    );
}

#[tokio::main]
async fn main() {
    RefugeWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/first_use.feature")
        .await;
}
