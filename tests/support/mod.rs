#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

use assert_cmd::Command;

#[derive(Debug)]
pub struct TestEnvironment {
    temp: tempfile::TempDir,
    pub config: PathBuf,
    pub repos: PathBuf,
    pub target: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    git_config: PathBuf,
}

impl TestEnvironment {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("test environment");
        let config = temp.path().join("config.toml");
        let repos = temp.path().join("repos");
        let target = temp.path().join("target");
        let home = temp.path().join("home");
        let xdg_config = temp.path().join("xdg-config");
        let git_config = temp.path().join("gitconfig");
        std::fs::create_dir_all(&home).expect("isolated HOME");
        std::fs::create_dir_all(&xdg_config).expect("isolated XDG config");
        std::fs::write(
            &git_config,
            "[user]\n\tname = Refuge Test\n\temail = refuge@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )
        .expect("isolated Git configuration");
        Self {
            temp,
            config,
            repos,
            target,
            home,
            xdg_config,
            git_config,
        }
    }

    pub fn path(&self) -> &Path {
        self.temp.path()
    }

    pub fn initialize(&self) {
        self.refuge()
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

    pub fn refuge(&self) -> Command {
        let mut command = Command::cargo_bin("refuge").expect("refuge binary");
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PATH", path_with_refuge())
            .env("REFUGE_CONFIG", &self.config);
        command
    }

    pub fn git_output(&self, repo: &Path, args: &[&str], with_refuge: bool) -> Output {
        let mut command = ProcessCommand::new("git");
        command
            .args(["-c", "safe.bareRepository=all"])
            .arg("-C")
            .arg(repo)
            .args(args);
        self.configure(&mut command, with_refuge);
        command.output().expect("run git")
    }

    pub fn git(&self, repo: &Path, args: &[&str], with_refuge: bool) -> String {
        let output = self.git_output(repo, args, with_refuge);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("UTF-8 Git output")
            .trim()
            .to_owned()
    }

    pub fn std_refuge(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("refuge"));
        self.configure(&mut command, true);
        command
    }

    fn configure(&self, command: &mut ProcessCommand, with_refuge: bool) {
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PATH", path_with_refuge());
        if with_refuge {
            command.env("REFUGE_CONFIG", &self.config);
        } else {
            command.env_remove("REFUGE_CONFIG");
        }
    }
}

pub fn path_with_refuge() -> OsString {
    let refuge_dir = assert_cmd::cargo::cargo_bin("refuge")
        .parent()
        .expect("refuge binary has a parent directory")
        .to_owned();
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![refuge_dir];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).expect("join PATH entries")
}
