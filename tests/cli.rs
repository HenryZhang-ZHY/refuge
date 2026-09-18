use assert_cmd::Command;
use predicates::str::contains;
use refuge::config::Config;

#[test]
fn prints_version() {
    Command::cargo_bin("refuge")
        .expect("refuge binary")
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("refuge {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn version_subcommand_prints_version_and_release_url() {
    Command::cargo_bin("refuge")
        .expect("refuge binary")
        .arg("version")
        .assert()
        .success()
        .stdout(format!(
            "refuge version {}\nhttps://github.com/HenryZhang-ZHY/refuge/releases/tag/v{}\n",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ));
}

#[test]
fn init_writes_config_and_creates_directories() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    let repos = temp.path().join("repos");
    let target = temp.path().join("target");

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config_path)
        .args([
            "init",
            "--repos",
            repos.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("initialized refuge"));

    let config = Config::load_from(&config_path).unwrap();
    assert_eq!(config.repos_dir, dunce::canonicalize(&repos).unwrap());
    assert_eq!(config.target_root, dunce::canonicalize(&target).unwrap());
    assert!(!config.instance_id.is_nil());
}

#[test]
fn init_defaults_repos_to_the_user_data_directory() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    let target = temp.path().join("target");
    let data_home = temp.path().join("data-home");

    let mut command = Command::cargo_bin("refuge").unwrap();
    command
        .env("REFUGE_CONFIG", &config_path)
        .args(["init", "--target", target.to_str().unwrap()]);
    #[cfg(windows)]
    command.env("LOCALAPPDATA", &data_home);
    #[cfg(not(windows))]
    command.env("XDG_DATA_HOME", &data_home);

    command.assert().success();

    let config = Config::load_from(&config_path).unwrap();
    let expected = dunce::canonicalize(data_home.join("refuge").join("repos")).unwrap();
    assert_eq!(config.repos_dir, expected);
}

#[test]
fn init_rejects_repository_directory_inside_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let repos = target.join("live-repositories");

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", temp.path().join("config.toml"))
        .args([
            "init",
            "--repos",
            repos.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("must not be inside the backup target"));
}

#[test]
fn init_rejects_repository_directory_inside_onedrive() {
    let temp = tempfile::tempdir().unwrap();
    let one_drive = temp.path().join("OneDrive - Example");
    let repos = one_drive.join("repositories");

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", temp.path().join("config.toml"))
        .env("OneDriveCommercial", &one_drive)
        .args([
            "init",
            "--repos",
            repos.to_str().unwrap(),
            "--target",
            temp.path().join("target").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("must not be inside a OneDrive directory"));
}

#[test]
fn init_refuses_to_replace_an_existing_instance() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    let repos = temp.path().join("repos");
    let target = temp.path().join("target");
    let arguments = [
        "init",
        "--repos",
        repos.to_str().unwrap(),
        "--target",
        target.to_str().unwrap(),
    ];

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config_path)
        .args(arguments)
        .assert()
        .success();
    let original = Config::load_from(&config_path).unwrap();

    Command::cargo_bin("refuge")
        .unwrap()
        .env("REFUGE_CONFIG", &config_path)
        .args(arguments)
        .assert()
        .code(2)
        .stderr(contains("Refuge is already initialized"))
        .stderr(contains("existing configuration was not changed"));

    assert_eq!(Config::load_from(&config_path).unwrap(), original);
}
