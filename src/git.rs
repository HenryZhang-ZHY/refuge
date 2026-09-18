use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefState {
    pub refs: BTreeMap<String, String>,
    pub head: Option<String>,
}

impl RefState {
    pub fn hash(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"refuge-ref-state-v1\0");
        if let Some(head) = &self.head {
            digest.update(b"HEAD\0");
            digest.update(head.as_bytes());
            digest.update(b"\0");
        }
        for (name, oid) in &self.refs {
            digest.update(name.as_bytes());
            digest.update(b"\0");
            digest.update(oid.as_bytes());
            digest.update(b"\0");
        }
        format!("sha256:{:x}", digest.finalize())
    }
}

fn output_error(args: &[&str], output: &Output) -> anyhow::Error {
    anyhow::anyhow!(
        "git {} failed with {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn run(repo: Option<&Path>, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    if let Some(repo) = repo {
        command.arg("-C").arg(repo);
    }
    let output = command
        .args(args)
        .output()
        .with_context(|| format!("could not run git {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(output_error(args, &output));
    }
    Ok(output)
}

pub fn ref_state(repo: &Path) -> Result<RefState> {
    let output = run(
        Some(repo),
        &["for-each-ref", "--format=%(refname)%00%(objectname)"],
    )?;
    let stdout = String::from_utf8(output.stdout).context("git returned non-UTF-8 ref data")?;
    let mut refs = BTreeMap::new();
    for line in stdout.lines() {
        let Some((name, oid)) = line.split_once('\0') else {
            bail!("git returned malformed ref data");
        };
        refs.insert(name.to_owned(), oid.to_owned());
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .context("could not read symbolic HEAD")?;
    let head = match output.status.code() {
        Some(0) => Some(
            String::from_utf8(output.stdout)
                .context("git returned non-UTF-8 HEAD")?
                .trim()
                .to_owned(),
        ),
        Some(1) => None,
        _ => return Err(output_error(&["symbolic-ref", "--quiet", "HEAD"], &output)),
    };

    Ok(RefState { refs, head })
}

pub fn bundle_create(repo: &Path, destination: &Path) -> Result<()> {
    let destination = destination
        .to_str()
        .context("bundle destination is not valid UTF-8")?;
    run(Some(repo), &["bundle", "create", destination, "--all"])?;
    Ok(())
}

pub fn bundle_verify(repo: &Path, bundle: &Path) -> Result<()> {
    let bundle = bundle.to_str().context("bundle path is not valid UTF-8")?;
    run(Some(repo), &["bundle", "verify", bundle])?;
    Ok(())
}

pub fn bundle_list_heads(bundle: &Path) -> Result<BTreeMap<String, String>> {
    let bundle = bundle.to_str().context("bundle path is not valid UTF-8")?;
    let output = run(None, &["bundle", "list-heads", bundle])?;
    let stdout = String::from_utf8(output.stdout).context("git returned non-UTF-8 bundle refs")?;
    let mut refs = BTreeMap::new();
    for line in stdout.lines() {
        let Some((oid, name)) = line.split_once(' ') else {
            bail!("git returned malformed bundle ref data");
        };
        if name != "HEAD" {
            refs.insert(name.to_owned(), oid.to_owned());
        }
    }
    Ok(refs)
}

pub fn clone_mirror(source: &Path, destination: &Path) -> Result<()> {
    let source = source.to_str().context("clone source is not valid UTF-8")?;
    let destination = destination
        .to_str()
        .context("clone destination is not valid UTF-8")?;
    run(None, &["clone", "--mirror", source, destination])?;
    Ok(())
}

pub fn init_bare(destination: &Path) -> Result<()> {
    let destination = destination
        .to_str()
        .context("repository destination is not valid UTF-8")?;
    run(
        None,
        &["init", "--bare", "--initial-branch=main", destination],
    )?;
    Ok(())
}

pub fn config_set(repo: &Path, key: &str, value: &str) -> Result<()> {
    run(Some(repo), &["config", key, value])?;
    Ok(())
}

pub fn config_get(repo: &Path, key: &str) -> Result<String> {
    let output = run(Some(repo), &["config", "--get", key])?;
    Ok(String::from_utf8(output.stdout)
        .context("git returned non-UTF-8 config data")?
        .trim()
        .to_owned())
}

pub fn set_symbolic_head(repo: &Path, target: &str) -> Result<()> {
    run(Some(repo), &["symbolic-ref", "HEAD", target])?;
    Ok(())
}
