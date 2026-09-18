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
your working copy.

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
refuge status            # all hosted repositories
refuge status notes      # a single repository
```

### 5. Inspect snapshots

```powershell
refuge snapshots list
refuge snapshots list notes
```

### 6. Restore

Restore the newest valid snapshot (or a specific one) from the target
directory, e.g. onto a fresh machine after `refuge init`:

```powershell
refuge restore notes
refuge restore notes --snapshot <SNAPSHOT_ID> --as recovered-notes
```

## Manual backup

Useful if you skipped the hook or want to force a fresh snapshot:

```powershell
refuge backup notes
```

## Notes

- Repository names may contain letters, digits, dots, dashes, and underscores.
- The `--repos` directory must never live inside a cloud-synced folder (e.g.
  OneDrive); Refuge checks for this and will refuse to initialize otherwise.
- See `docs/product/2026-09-18-refuge-prd.md` for the full product rationale.
