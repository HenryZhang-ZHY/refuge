# Refuge

Local-first Git repository backup. Refuge hosts live bare Git repositories
outside your sync folder and publishes verified, immutable snapshots into a
plain directory (e.g. a OneDrive/Dropbox/iCloud folder). Your sync client is
responsible for uploading that directory — Refuge does not verify cloud
upload, only the local snapshot.

## Install / Build

```powershell
git clone <this-repo>
cd refuge
cargo build --release
# binary at target/release/refuge(.exe)
```

Put the `refuge` binary somewhere on your `PATH`. The post-receive hook that
runs on every push resolves `refuge` through `PATH` at push time (it does not
bake in an absolute path), so backups keep working across rebuilds or moves
as long as `refuge` stays on `PATH`. If it isn't found, the push still
succeeds but prints a clear warning that the snapshot was NOT backed up.

Check the installed CLI version with:

```powershell
refuge version
```

## Docker server

Server mode is a single-container, single-owner Git host with an embedded Web UI.
It exposes standard Git Smart HTTP and Git LFS endpoints; client machines need
only `git` and, for LFS repositories, `git-lfs`. There is no Refuge-specific Git
transport.

Create the portable backup directory and a fixed owner key, then start the
included Compose stack:

```sh
install -d -m 700 refuge-backup
openssl rand -hex 32 > refuge-secret.txt
chmod 600 refuge-secret.txt
chown -R 10001:10001 refuge-backup
docker compose up --build -d
```

Open <http://127.0.0.1:7788>, sign in with that key, and create a repository.
The dashboard displays a copyable standard clone command:

```sh
git clone http://127.0.0.1:7788/git/notes.git
```

When Git asks for credentials, use `refuge` as the username and the same owner
key as the password. A Git credential helper can store it normally. The key is
also used by the Web UI to obtain an HttpOnly session cookie and is not stored
in browser local storage.

Server storage deliberately has two different lifecycles:

- `/var/lib/refuge` is local runtime data: live bare repositories, the backup
  queue, and runtime identity. The supplied Compose file uses a Docker-managed
  volume so Git repositories have stable container ownership.
- `/var/backups/refuge` is the portable backup boundary. The supplied Compose
  file bind-mounts `./refuge-backup` there; this is the only directory that must
  be synchronized or copied for disaster recovery.

On a new server, mount the preserved backup directory and start Refuge with an
empty runtime volume. Refuge verifies and restores every repository from its
newest valid snapshot before accepting traffic. There is no live Git repository
inside the bind mount, so host/container UID differences cannot trigger Git's
repository ownership checks. UID 10001 still needs ordinary read/write access
to the backup directory.

The two layouts are:

```text
Docker volume: /var/lib/refuge/
├── runtime.toml
├── repos/
└── queue/

Portable bind mount: /var/backups/refuge/
└── refuge/v2/repos/…
```

The owner key remains a deployment secret rather than repository data. Preserve
it independently in a password manager or secret manager; it does not belong in
the portable backup directory.

The default port mapping is loopback-only. Put Caddy, Tailscale Serve, or
another TLS terminator in front before exposing the service to other machines.
The server itself deliberately serves HTTP inside that trusted boundary.

## Getting Started

### 1. Initialize Refuge

Pick a `--target`: the sync-client folder (e.g. your OneDrive path) that will
receive snapshots. `--repos` (the local, non-cloud-synced directory that will
hold your live bare repositories) is optional — it defaults to your user data
directory (`~/.local/share/refuge/repos` on Linux/macOS, `%LOCALAPPDATA%\refuge\repos`
on Windows), which is never inside a cloud sync folder.

```powershell
refuge init --target "C:\Users\<you>\OneDrive\refuge"
# or, to choose your own repositories directory:
refuge init --repos D:\git\refuge-repos --target "C:\Users\<you>\OneDrive\refuge"
```

This writes a config file (`%APPDATA%\refuge\config.toml` on Windows,
`$XDG_CONFIG_HOME/refuge/config.toml` or `~/.config/refuge/config.toml` on
Linux/macOS). Run `init` only once per machine.

### 2. Host a repository

Create a new empty hosted repo, or import an existing one:

```powershell
refuge repo create notes
# or
refuge repo import notes D:\path\to\existing\repo
```

Either command prints a `git remote add refuge ...` command — run it inside
your working copy. Creation and import immediately publish an initial verified
snapshot, so the repository is protected before its first subsequent push.

For a new repository, Refuge can also create a working copy immediately:

```powershell
refuge repo create notes --clone
# or choose the working-copy directory:
refuge repo create notes --clone D:\work\notes
```

The import source defaults to the current directory. `--connect` additionally
adds a `refuge` remote to that working tree:

```powershell
cd D:\work\notes
refuge repo import notes --connect
```

### 3. Push to back it up

```powershell
git remote add refuge "D:\git\refuge-repos\notes.git"
git push refuge main
```

Every push runs a post-receive hook that publishes a verified recovery point to
your `--target` directory. The first non-empty recovery point is a full Git
bundle checkpoint; later pushes normally add only an incremental bundle. The
manifest is written last and is the publication marker. No separate backup step
is needed for normal use.

### 4. Check status

```powershell
refuge repo status --all  # all hosted repositories
refuge repo status notes  # a single repository
```

### 5. Inspect snapshots

```powershell
refuge snapshots list
refuge snapshots list notes
refuge snapshots verify notes
refuge snapshots usage notes
```

### 6. Create another working copy

List hosted repositories and clone one by name or stable repository ID:

```powershell
refuge repo list
refuge repo clone notes
refuge repo clone notes D:\work\notes
```

`repo clone` creates the standard `origin` remote. Additional `git clone`
arguments may be supplied after `--`, for example:

```powershell
refuge repo clone notes -- --single-branch
```

To connect an existing working copy without importing it again:

```powershell
cd D:\work\notes
refuge repo connect notes
```

This adds a remote named `refuge` without replacing an existing remote. Use
`--remote <NAME>` to choose another name, or `--replace` to explicitly update
an existing remote with that name.

Inside a connected working copy, `.` selects the associated hosted repository:

```powershell
refuge repo view
refuge repo status
refuge repo backup
```

### 7. Restore

Restore the newest valid snapshot (or a specific one) from the target
directory, e.g. onto a fresh machine after `refuge init`:

```powershell
refuge restore notes
refuge restore notes --snapshot <SNAPSHOT_ID> --as recovered-notes
```

## Manual backup

Useful if you skipped the hook. It is idempotent when the current refs are
already protected. Use `--checkpoint` to deliberately publish a new full
checkpoint:

```powershell
refuge repo backup notes
refuge repo backup notes --checkpoint
```

## Git LFS support

Refuge automatically backs up Git LFS content too. Reachable LFS pointers are
scanned in batches, and each required object is stored once under its SHA-256
OID. A small content-addressed set file records the exact objects required by a
recovery point. Git-only pushes therefore write zero new LFS object bytes.
Restore hashes every copied object and independently derives the expected set
from the restored Git history. No extra configuration is needed.

## Notes

- Repository names may contain letters, digits, dots, dashes, and underscores.
- The `--repos` directory must not live inside the selected backup target.
  Refuge also rejects paths under OneDrive roots exposed through the current
  process environment. That detection is a guardrail, not proof that an
  arbitrary directory is never synchronized; choose a known local-only path.
- See `docs/product/2026-09-18-refuge-prd.md` for the full product rationale.

## Development

The release test suite requires stable Rust, Git, and Git LFS. It intentionally
fails instead of skipping the real LFS backup/restore scenarios when `git-lfs`
is unavailable.

```sh
git lfs version
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI runs the same checks on Linux and Windows. Test subprocesses must use the
isolated helpers in `tests/support` so they never read or modify a developer's
real home directory or global/system Git configuration.
