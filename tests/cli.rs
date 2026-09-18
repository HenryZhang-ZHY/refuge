use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn prints_version() {
    Command::cargo_bin("refuge")
        .expect("refuge binary")
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("refuge {}", env!("CARGO_PKG_VERSION"))));
}
