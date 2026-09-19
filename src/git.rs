use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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

fn command(repo: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    // Git hooks export repository-location variables for the repository that
    // invoked the hook. Refuge frequently targets a different repository via
    // `-C`, so inheriting those variables can silently redirect the command.
    // Authentication helpers, HOME, and ordinary Git configuration remain
    // inherited intentionally.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
    ] {
        command.env_remove(key);
    }
    if let Some(repo) = repo {
        command
            .args(["-c", "safe.bareRepository=all"])
            .arg("-C")
            .arg(repo);
    }
    command
}

fn run(repo: Option<&Path>, args: &[&str]) -> Result<Output> {
    let mut command = command(repo);
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

    let output = command(Some(repo))
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

pub const MAX_EXCLUSIONS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchCheck {
    Found {
        oid: String,
        kind: String,
        size: u64,
    },
    Missing,
}

fn stdin_file(
    repo: &Path,
    lines: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<tempfile::NamedTempFile> {
    let staging = repo.parent().unwrap_or(repo).join(".refuge-staging");
    std::fs::create_dir_all(&staging)?;
    let mut file = tempfile::NamedTempFile::new_in(staging)?;
    for line in lines {
        writeln!(file, "{}", line.as_ref())?;
    }
    file.flush()?;
    Ok(file)
}

pub fn batch_check(repo: &Path, names: &[String]) -> Result<Vec<BatchCheck>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let input = stdin_file(repo, names)?;
    let output = command(Some(repo))
        .args(["cat-file", "--batch-check"])
        .stdin(Stdio::from(input.reopen()?))
        .output()
        .context("could not run git cat-file --batch-check")?;
    if !output.status.success() {
        return Err(output_error(&["cat-file", "--batch-check"], &output));
    }
    let stdout = String::from_utf8(output.stdout).context("git returned non-UTF-8 batch data")?;
    let lines = stdout.lines().collect::<Vec<_>>();
    if lines.len() != names.len() {
        bail!("git cat-file returned the wrong number of batch results");
    }
    lines
        .into_iter()
        .map(|line| {
            if line.ends_with(" missing") {
                return Ok(BatchCheck::Missing);
            }
            let mut fields = line.split_whitespace();
            let oid = fields
                .next()
                .context("malformed batch-check oid")?
                .to_owned();
            let kind = fields
                .next()
                .context("malformed batch-check kind")?
                .to_owned();
            let size = fields
                .next()
                .context("malformed batch-check size")?
                .parse()
                .context("invalid batch-check size")?;
            if fields.next().is_some() {
                bail!("malformed batch-check result");
            }
            Ok(BatchCheck::Found { oid, kind, size })
        })
        .collect()
}

pub fn batch_blob_contents(repo: &Path, oids: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let input = stdin_file(repo, oids)?;
    let output = command(Some(repo))
        .args(["cat-file", "--batch"])
        .stdin(Stdio::from(input.reopen()?))
        .output()
        .context("could not run git cat-file --batch")?;
    if !output.status.success() {
        return Err(output_error(&["cat-file", "--batch"], &output));
    }
    let mut cursor = 0usize;
    let mut results = Vec::with_capacity(oids.len());
    for _ in oids {
        let end = output.stdout[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .context("malformed batch header")?
            + cursor;
        let header =
            std::str::from_utf8(&output.stdout[cursor..end]).context("non-UTF-8 batch header")?;
        if header.ends_with(" missing") {
            bail!("Git blob object is missing");
        }
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            bail!("Git object is not a blob");
        }
        let size: usize = fields[2].parse().context("invalid batch blob size")?;
        cursor = end + 1;
        let content_end = cursor
            .checked_add(size)
            .context("batch blob size overflow")?;
        if content_end >= output.stdout.len() || output.stdout[content_end] != b'\n' {
            bail!("truncated batch blob result");
        }
        results.push((
            fields[0].to_owned(),
            output.stdout[cursor..content_end].to_vec(),
        ));
        cursor = content_end + 1;
    }
    if cursor != output.stdout.len() {
        bail!("unexpected trailing batch blob output");
    }
    Ok(results)
}

pub fn bundle_create(repo: &Path, destination: &Path, exclusions: &[String]) -> Result<()> {
    if exclusions.len() > MAX_EXCLUSIONS {
        bail!("too many bundle exclusions");
    }
    let destination = destination
        .to_str()
        .context("bundle destination is not valid UTF-8")?;
    let mut args = vec![
        "bundle".to_owned(),
        "create".to_owned(),
        destination.to_owned(),
        "--all".to_owned(),
    ];
    args.extend(exclusions.iter().map(|oid| format!("^{oid}")));
    let references = args.iter().map(String::as_str).collect::<Vec<_>>();
    run(Some(repo), &references)?;
    Ok(())
}

pub fn has_objects_outside(repo: &Path, exclusions: &[String]) -> Result<bool> {
    if exclusions.len() > MAX_EXCLUSIONS {
        bail!("too many object exclusions");
    }
    let mut args = vec![
        "rev-list".to_owned(),
        "--objects".to_owned(),
        "--all".to_owned(),
    ];
    args.extend(exclusions.iter().map(|oid| format!("^{oid}")));
    let references = args.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(!run(Some(repo), &references)?.stdout.is_empty())
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

pub fn bundle_unbundle(repo: &Path, bundle: &Path) -> Result<()> {
    bundle_verify(repo, bundle)?;
    let bundle = bundle.to_str().context("bundle path is not valid UTF-8")?;
    run(Some(repo), &["bundle", "unbundle", bundle])?;
    Ok(())
}

pub fn create_refs(repo: &Path, refs: &BTreeMap<String, String>) -> Result<()> {
    let lines = refs
        .iter()
        .map(|(name, oid)| format!("create {name} {oid}"));
    let input = stdin_file(repo, lines)?;
    let output = command(Some(repo))
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::from(input.reopen()?))
        .output()
        .context("could not run git update-ref --stdin")?;
    if !output.status.success() {
        return Err(output_error(&["update-ref", "--stdin"], &output));
    }
    Ok(())
}

pub fn clone_mirror(source: &Path, destination: &Path) -> Result<()> {
    let source = source.to_str().context("clone source is not valid UTF-8")?;
    let destination = destination
        .to_str()
        .context("clone destination is not valid UTF-8")?;
    run(None, &["clone", "--mirror", source, destination])?;
    Ok(())
}

pub fn clone_working(
    source: &Path,
    destination: &Path,
    remote_name: &str,
    extra_args: &[OsString],
) -> Result<()> {
    let output = command(None)
        .arg("clone")
        .args(extra_args)
        .args(["--origin", remote_name])
        .arg(source)
        .arg(destination)
        .output()
        .context("could not run git clone")?;
    if !output.status.success() {
        bail!(
            "git clone failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
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

pub fn config_get_optional(repo: &Path, key: &str) -> Result<Option<String>> {
    let output = command(Some(repo))
        .args(["config", "--get", key])
        .output()
        .with_context(|| format!("could not read git config {key}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(output.stdout)
                .context("git returned non-UTF-8 config data")?
                .trim()
                .to_owned(),
        )),
        Some(1) => Ok(None),
        _ => Err(output_error(&["config", "--get", key], &output)),
    }
}

pub fn top_level(repo: &Path) -> Result<std::path::PathBuf> {
    let output = run(Some(repo), &["rev-parse", "--show-toplevel"])?;
    Ok(std::path::PathBuf::from(
        String::from_utf8(output.stdout)
            .context("git returned a non-UTF-8 working tree path")?
            .trim(),
    ))
}

pub fn remotes(repo: &Path) -> Result<Vec<(String, String)>> {
    let output = run(Some(repo), &["remote"])?;
    let names = String::from_utf8(output.stdout).context("git returned non-UTF-8 remote names")?;
    names
        .lines()
        .map(|name| {
            let url = config_get(repo, &format!("remote.{name}.url"))?;
            Ok((name.to_owned(), url))
        })
        .collect()
}

pub fn remote_add(repo: &Path, name: &str, url: &Path) -> Result<()> {
    let url = url.to_str().context("remote path is not valid UTF-8")?;
    run(Some(repo), &["remote", "add", name, url])?;
    Ok(())
}

pub fn remote_set_url(repo: &Path, name: &str, url: &Path) -> Result<()> {
    let url = url.to_str().context("remote path is not valid UTF-8")?;
    run(Some(repo), &["remote", "set-url", name, url])?;
    Ok(())
}

pub fn set_symbolic_head(repo: &Path, target: &str) -> Result<()> {
    run(Some(repo), &["symbolic-ref", "HEAD", target])?;
    Ok(())
}

pub fn fsck(repo: &Path) -> Result<()> {
    run(Some(repo), &["fsck", "--full", "--strict"])?;
    Ok(())
}

pub fn reachable_objects(repo: &Path) -> Result<Vec<String>> {
    let output = run(Some(repo), &["rev-list", "--objects", "--all"])?;
    let stdout = String::from_utf8(output.stdout).context("git returned non-UTF-8 object data")?;
    Ok(stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect())
}

pub fn object_type(repo: &Path, oid: &str) -> Result<String> {
    let output = run(Some(repo), &["cat-file", "-t", oid])?;
    Ok(String::from_utf8(output.stdout)
        .context("git returned a non-UTF-8 object type")?
        .trim()
        .to_owned())
}

pub fn object_size(repo: &Path, oid: &str) -> Result<u64> {
    let output = run(Some(repo), &["cat-file", "-s", oid])?;
    String::from_utf8(output.stdout)
        .context("git returned a non-UTF-8 object size")?
        .trim()
        .parse()
        .context("git returned an invalid object size")
}

pub fn object_contents(repo: &Path, oid: &str) -> Result<Vec<u8>> {
    Ok(run(Some(repo), &["cat-file", "blob", oid])?.stdout)
}
