use assert_cmd::Command;
use predicates::prelude::*;

fn refuge() -> Command {
    Command::cargo_bin("refuge").unwrap()
}

#[test]
fn no_arguments_prints_quick_start_and_returns_usage_error() {
    refuge()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Typical workflow"))
        .stderr(predicate::str::contains(
            "refuge init --target <SYNC_DIR>",
        ))
        .stderr(predicate::str::contains("refuge status <NAME>"));
}

#[test]
fn root_help_explains_the_complete_daily_workflow() {
    refuge()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Typical workflow"))
        .stdout(predicate::str::contains("git push refuge main"))
        .stdout(predicate::str::contains("Cloud upload is not verified"));
}

#[test]
fn command_help_describes_values_and_gives_examples() {
    refuge()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--repos <LOCAL_DIR>"))
        .stdout(predicate::str::contains("--target <SYNC_DIR>"));

    refuge()
        .args(["repo", "create", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<NAME>  Repository name"))
        .stdout(predicate::str::contains("refuge repo create notes"));

    refuge()
        .args(["restore", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "<NAME_OR_REPO_ID>  Repository name or stable UUID",
        ))
        .stdout(predicate::str::contains("--replace"))
        .stdout(predicate::str::contains(
            "Replace an existing hosted repository",
        ))
        .stdout(predicate::str::contains("refuge restore notes"));
}

#[test]
fn init_requires_the_user_to_choose_a_backup_target() {
    let temp = tempfile::tempdir().unwrap();
    refuge()
        .env("REFUGE_CONFIG", temp.path().join("config.toml"))
        .args([
            "init",
            "--repos",
            temp.path().join("repos").to_str().unwrap(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the following required arguments were not provided",
        ))
        .stderr(predicate::str::contains("--target <SYNC_DIR>"));
}

#[test]
fn mistyped_command_suggests_the_valid_command() {
    refuge()
        .arg("stats")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unrecognized subcommand 'stats'"))
        .stderr(predicate::str::contains(
            "similar subcommand exists: 'status'",
        ));
}

#[test]
fn conflicting_backup_arguments_explain_the_conflict() {
    refuge()
        .args(["backup", "notes", "--repo-path", "."])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the argument '[NAME]' cannot be used with '--repo-path <BARE_REPO>'",
        ))
        .stderr(predicate::str::contains("Usage: refuge backup"));
}

#[test]
fn missing_configuration_names_refuge_and_points_to_init() {
    let temp = tempfile::tempdir().unwrap();
    refuge()
        .env("REFUGE_CONFIG", temp.path().join("missing.toml"))
        .arg("status")
        .assert()
        .code(2)
        .stderr(predicate::str::starts_with("refuge: "))
        .stderr(predicate::str::contains("Refuge is not initialized"))
        .stderr(predicate::str::contains("refuge init --repos"));
}
