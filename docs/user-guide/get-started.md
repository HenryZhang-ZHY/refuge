# Get Started

## Why Refuge?

When backing up Git repositories using cloud drives (like OneDrive or Dropbox), placing active Git repositories or working trees directly inside a sync folder often leads to file conflicts, corrupted Git databases, and unnecessary syncing of temporary files (`node_modules`, `.venv`, caches).

**Refuge** separates the active Git repository from the cloud sync folder:

1. **Local Bare Repository (Live Host):** Refuge hosts a local bare Git repository on your machine outside any sync folder. You work in your normal working tree and interact with it using regular `git push` / `git pull`.
2. **Immutable Snapshot Target (Cloud Backup):** On every push, Refuge automatically verifies your repository and exports an immutable snapshot (bundle + manifest) to your cloud sync folder.
3. **Disaster Recovery:** If your machine crashes, you can reconstruct your repositories on a clean machine directly from that cloud backup directory.

```text
[ Working Copy ] ──git push──> [ Local Bare Repo (Refuge) ] ──auto hook──> [ Cloud Sync Folder (Target) ]
(your active code)             (outside cloud sync)                        (immutable backup snapshots)
```

---

## 1. Initial Setup (One-time)

Initialize Refuge once per machine by specifying the cloud sync folder that will receive backup snapshots:

```bash
refuge init --target "/path/to/sync-folder"
```

> **Note:** By default, Refuge stores live bare repositories safely in your user data directory (never inside cloud sync). You can customize this location with `--repos <DIR>`.

---

## 2. Get Your Repository into Refuge

Choose either **Option A** for an existing project or **Option B** for a fresh start.

### Option A: Import an Existing Local Repository

If you already have a local Git repository:

```bash
cd /path/to/your/project
refuge repo import <repo-name> --connect
```

- Creates a hosted bare repository managed by Refuge.
- Adds `refuge` as a remote to your current working tree (`git remote -v`).
- Takes an initial verified snapshot immediately.

### Option B: Create a New Repository

If you want to start a new project:

```bash
refuge repo create <repo-name> --clone
```

- Creates a hosted bare repository in Refuge.
- Clones a working copy into `./<repo-name>` with an `origin` remote configured.
- `cd <repo-name>` to begin working right away.

---

## 3. Daily Workflow: Just `git push`

Work with Git as you normally do. Whenever you push to the Refuge remote, Refuge automatically publishes a verified snapshot to your cloud sync folder:

```bash
git push refuge main
# or git push origin main if created via --clone
```

No manual backup commands required!

### Check Backup Status

Verify that your local repository matches the latest snapshot in your backup target:

```bash
refuge repo status
```

Or view status across all hosted repositories:

```bash
refuge repo status --all
```

---

## 4. Disaster Recovery

To restore a repository from your cloud sync folder onto a new or recovered machine:

1. Initialize Refuge on the new machine:

   ```bash
   refuge init --target "/path/to/sync-folder"
   ```

2. Restore the repository from verified snapshots:

   ```bash
   refuge restore <repo-name>
   ```

3. Clone and continue working:

   ```bash
   refuge repo clone <repo-name>
   ```
