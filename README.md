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

Every push runs a post-receive hook that automatically publishes a verified
snapshot (bundle + manifest) to your `--target` directory. No separate backup
step is needed for normal use.

### 4. Check status

```powershell
refuge repo status --all  # all hosted repositories
refuge repo status notes  # a single repository
```

### 5. Inspect snapshots

```powershell
refuge snapshots list
refuge snapshots list notes
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

Useful if you skipped the hook or want to force a fresh snapshot:

```powershell
refuge repo backup notes
```

## Git LFS support

Refuge automatically backs up Git LFS content too. If a hosted bare repo has
LFS objects under `lfs/objects` (e.g. because contributors pushed with
`git-lfs` installed), every backup/restore also snapshots and restores that
directory as a separate archive alongside the git bundle — no extra
configuration needed. `refuge repo status`/`refuge repo backup` output shows
the LFS byte count when present.

## Notes

- Repository names may contain letters, digits, dots, dashes, and underscores.
- The `--repos` directory must never live inside a cloud-synced folder (e.g.
  OneDrive); Refuge checks for this and will refuse to initialize otherwise.
- See `docs/product/2026-09-18-refuge-prd.md` for the full product rationale.
