use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use refuge::git;

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        // Newer git defaults to `safe.bareRepository = explicit`, which
        // refuses to auto-detect a bare repository via `-C`. This helper is
        // used against bare hosted repos, so opt back in explicitly.
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
fn repository_listing_rejects_duplicate_active_identities() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    for name in ["first", "second"] {
        Command::cargo_bin("refuge")
            .unwrap()
            .env("REFUGE_CONFIG", &config)
            .args(["repo", "create", name])
            .assert()
            .success();
    }
    let id = git_output(&repos.join("first.git"), &["config", "refuge.repoid"]);
    git_output(&repos.join("second.git"), &["config", "refuge.repoid", &id]);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "list"])
        .assert()
        .failure()
        .stderr(contains("duplicate refuge.repoid"));
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
fn repo_import_copies_lfs_objects_before_initial_backup() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git_output(&source, &["init", "--initial-branch=main"]);
    git_output(&source, &["config", "user.name", "Refuge Test"]);
    git_output(&source, &["config", "user.email", "refuge@example.invalid"]);
    git_output(&source, &["lfs", "install", "--local"]);
    let storage = temp.path().join("custom-lfs-storage");
    git_output(
        &source,
        &["config", "lfs.storage", storage.to_str().unwrap()],
    );
    git_output(&source, &["lfs", "track", "*.bin"]);
    let contents = b"file stored outside the Git object database\n";
    std::fs::write(source.join("asset.bin"), contents).unwrap();
    git_output(&source, &["add", "."]);
    git_output(&source, &["commit", "-m", "add LFS asset"]);
    std::fs::write(
        source.join("asset.bin"),
        b"second version of the LFS asset\n",
    )
    .unwrap();
    git_output(&source, &["add", "asset.bin"]);
    git_output(&source, &["commit", "-m", "update LFS asset"]);
    let media = git_output(&source, &["lfs", "env"])
        .lines()
        .find_map(|line| line.strip_prefix("LocalMediaDir="))
        .map(PathBuf::from)
        .unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "import", "notes", source.to_str().unwrap()])
        .assert()
        .success();

    let imported = repos.join("notes.git");
    let set = refuge::lfs::required_set(&imported).unwrap();
    assert_eq!(set.len(), 2);
    for (oid, size) in set {
        let relative = Path::new(&oid[..2]).join(&oid[2..4]).join(&oid);
        let content = std::fs::read(refuge::lfs::local_object_path(&imported, &oid)).unwrap();
        assert_eq!(content, std::fs::read(media.join(relative)).unwrap());
        assert_eq!(size, content.len() as u64);
    }
}

#[test]
fn repo_import_rejects_missing_lfs_objects_before_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git_output(&source, &["init", "--initial-branch=main"]);
    git_output(&source, &["config", "user.name", "Refuge Test"]);
    git_output(&source, &["config", "user.email", "refuge@example.invalid"]);
    git_output(&source, &["lfs", "install", "--local"]);
    git_output(&source, &["lfs", "track", "*.bin"]);
    std::fs::write(source.join("asset.bin"), b"missing object\n").unwrap();
    git_output(&source, &["add", "."]);
    git_output(&source, &["commit", "-m", "add LFS asset"]);
    let (oid, _) = refuge::lfs::required_set(&source)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let object = refuge::lfs::local_object_path(&source.join(".git"), &oid);
    std::fs::remove_file(object).unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "import", "notes", source.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("required source LFS object"));
    assert!(!repos.join("notes.git").exists());
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
        .stderr(contains("invalid repository name"))
        .stderr(contains(
            "use letters, digits, dots, dashes, or underscores",
        ));
}

#[test]
fn repo_list_and_clone_make_hosted_repositories_available_as_working_copies() {
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

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "import", "notes", source.to_str().unwrap()])
        .assert()
        .success();

    let hosted = repos.join("notes.git");
    let repo_id = git_output(&hosted, &["config", "refuge.repoid"]);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "list"])
        .assert()
        .success()
        .stdout(contains("notes"))
        .stdout(contains(&repo_id));

    let clone = temp.path().join("working-notes");
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args([
            "repo",
            "clone",
            &repo_id,
            clone.to_str().unwrap(),
            "--",
            "--single-branch",
        ])
        .assert()
        .success()
        .stdout(contains("cloned notes"));

    assert_eq!(
        git_output(&clone, &["remote", "get-url", "origin"]),
        hosted.display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(clone.join("entry.txt"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["one"]
    );
}

#[test]
fn connect_and_dot_selector_operate_on_the_current_working_copy() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    git_output(&work, &["init", "--initial-branch=main"]);
    git_output(&work, &["config", "user.name", "Refuge Test"]);
    git_output(&work, &["config", "user.email", "refuge@example.invalid"]);
    std::fs::write(work.join("entry.txt"), "one\n").unwrap();
    git_output(&work, &["add", "entry.txt"]);
    git_output(&work, &["commit", "-m", "initial"]);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "import", "notes", work.to_str().unwrap()])
        .assert()
        .success();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "connect", "notes"])
        .assert()
        .success()
        .stdout(contains("connected remote `refuge`"));
    // Reconnecting the same remote is deliberately idempotent.
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "connect", "notes"])
        .assert()
        .success();
    assert_eq!(
        git_output(&work, &["remote", "get-url", "refuge"]),
        repos.join("notes.git").display().to_string()
    );
    git_output(
        &work,
        &[
            "remote",
            "add",
            "backup",
            repos.join("notes.git").to_str().unwrap(),
        ],
    );

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "view"])
        .assert()
        .success()
        .stdout(contains("Name: notes"))
        .stdout(contains("Remote: refuge"));
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "backup"])
        .assert()
        .success();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "status"])
        .assert()
        .success()
        .stdout(contains("notes: Protected locally"));
}

#[test]
fn connect_refuses_to_replace_an_existing_remote_without_the_flag() {
    let temp = tempfile::tempdir().unwrap();
    let (config, _) = initialized(&temp);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "create", "notes"])
        .assert()
        .success();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    git_output(&work, &["init", "--initial-branch=main"]);
    git_output(&work, &["remote", "add", "refuge", "/somewhere/else"]);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "connect", "notes"])
        .assert()
        .failure()
        .stderr(contains("already exists"))
        .stderr(contains("--replace"));

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&work)
        .args(["repo", "connect", "notes", "--replace"])
        .assert()
        .success();
}

#[test]
fn create_can_clone_the_new_hosted_repository_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(temp.path())
        .args(["repo", "create", "scratch", "--clone"])
        .assert()
        .success()
        .stdout(contains("cloned scratch"));

    let work = temp.path().join("scratch");
    assert_eq!(
        git_output(&work, &["remote", "get-url", "origin"]),
        repos.join("scratch.git").display().to_string()
    );
}

#[test]
fn import_can_connect_the_source_and_publish_its_initial_snapshot() {
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

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args([
            "repo",
            "import",
            "notes",
            source.to_str().unwrap(),
            "--connect",
        ])
        .assert()
        .success()
        .stdout(contains("connected remote `refuge`"))
        .stdout(contains("protected"));

    assert_eq!(
        git_output(&source, &["remote", "get-url", "refuge"]),
        repos.join("notes.git").display().to_string()
    );
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "status", "notes"])
        .assert()
        .success()
        .stdout(contains("notes: Protected locally"));
}

#[test]
fn repo_status_all_lists_every_hosted_repository() {
    let temp = tempfile::tempdir().unwrap();
    let (config, _) = initialized(&temp);
    for name in ["notes", "ledger"] {
        Command::cargo_bin("refuge")
            .unwrap()
            .env("REFUGE_CONFIG", &config)
            .args(["repo", "create", name])
            .assert()
            .success();
    }

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "status", "--all"])
        .assert()
        .success()
        .stdout(contains("ledger: Protected locally"))
        .stdout(contains("notes: Protected locally"));
}

#[test]
fn import_defaults_to_current_directory_and_publishes_initial_snapshot() {
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

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .current_dir(&source)
        .args(["repo", "import", "notes"])
        .assert()
        .success()
        .stdout(contains("protected"));

    assert!(repos.join("notes.git").is_dir());
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "status", "notes"])
        .assert()
        .success()
        .stdout(contains("notes: Protected locally"));
}

fn import_committed(temp: &tempfile::TempDir, config: &Path, name: &str) {
    let source = temp.path().join(format!("{name}-source"));
    std::fs::create_dir(&source).unwrap();
    git_output(&source, &["init", "--initial-branch=main"]);
    git_output(&source, &["config", "user.name", "Refuge Test"]);
    git_output(&source, &["config", "user.email", "refuge@example.invalid"]);
    std::fs::write(source.join("entry.txt"), "one\n").unwrap();
    git_output(&source, &["add", "entry.txt"]);
    git_output(&source, &["commit", "-m", "initial"]);
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", config)
        .args(["repo", "import", name, source.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn repo_remove_deletes_live_repository_but_preserves_snapshots_for_restore() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    import_committed(&temp, &config, "notes");
    let hosted = repos.join("notes.git");
    let expected = git::ref_state(&hosted).unwrap();
    let repo_id = git_output(&hosted, &["config", "refuge.repoid"]);
    let snapshots = temp
        .path()
        .join("target")
        .join(refuge::layout::TARGET_ROOT)
        .join(&repo_id)
        .join("snapshots");
    let before = std::fs::read_dir(&snapshots).unwrap().count();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "remove", "notes"])
        .assert()
        .success()
        .stdout(contains("removed repository notes"))
        .stdout(contains("refuge restore notes"));

    assert!(!hosted.exists());
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "list"])
        .assert()
        .success()
        .stdout(contains("notes").not());
    assert_eq!(std::fs::read_dir(&snapshots).unwrap().count(), before);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["restore", "notes"])
        .assert()
        .success();
    assert_eq!(git::ref_state(&hosted).unwrap().refs, expected.refs);
}

#[test]
fn repo_remove_refuses_changes_missing_from_the_newest_snapshot_unless_forced() {
    let temp = tempfile::tempdir().unwrap();
    let (config, repos) = initialized(&temp);
    import_committed(&temp, &config, "notes");
    let hosted = repos.join("notes.git");
    git_output(&hosted, &["branch", "unsaved", "main"]);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "remove", "notes"])
        .assert()
        .failure()
        .stderr(contains("refuge repo backup notes"))
        .stderr(contains("--force"));
    assert!(hosted.is_dir());

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "remove", "notes", "--force"])
        .assert()
        .success();
    assert!(!hosted.exists());
}

#[test]
fn repo_remove_requires_an_explicit_existing_repository() {
    let temp = tempfile::tempdir().unwrap();
    let (config, _) = initialized(&temp);

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "remove"])
        .assert()
        .failure();
    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config)
        .args(["repo", "remove", "missing"])
        .assert()
        .failure()
        .stderr(contains("repository does not exist: missing"));
}
