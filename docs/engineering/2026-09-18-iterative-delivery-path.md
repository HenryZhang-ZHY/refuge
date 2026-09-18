---
title: Refuge Iterative Delivery Path
status: Active
date: 2026-09-18
supersedes_ordering_of: docs/engineering/2026-09-18-implementation-plan.md
---

# Refuge Iterative Delivery Path

Goal: a version the author uses for real work **today** (Persona A: Windows laptop, company
OneDrive), then grow it in small steps without ever breaking already-published backups.

The technical review still stands. What changes is the order and what is deferred:

- **Upload confirmation is the user's job.** Refuge writes a verified artifact into the OneDrive
  sync folder; the OneDrive client uploads it. No Cloud Files probe, no `PublishedUnconfirmed`
  state. Status reports `Protected (local copy in target; upload owned by OneDrive)`. The Graph
  adapter, if it ever ships, is where strict confirmation would live.
- **Synchronous backup first.** The `post-receive` hook runs the backup inline. For the target
  repositories (Beancount, Obsidian) a full bundle takes well under a second, and writing to the
  sync folder is a local disk write. A hook failure cannot fail the push because refs are already
  updated when `post-receive` runs, so PRD §5.4 is preserved. The daemon comes in iteration 1.
- **No database.** Protection state is derivable: current ref-state hash vs the newest manifest in
  the target. Repository identity lives in the bare repo's own config (`refuge.repoid`), so the
  registry is "scan the repos directory". SQLite arrives only when the async queue needs it.

Two things are **not** cut, because they become compatibility contracts the moment the first
real backup lands:

1. Manifest v1 schema (public envelope shape, snapshot id format, remote layout).
2. Stable `RepoId` (UUID v7) recorded in the bare repo and in every manifest.

## Iteration 0: usable today

Single crate, single binary, ~500 lines. No async, no SQLite, no encryption, no network.

### Commands

```text
refuge init [--repos <dir>] [--target <onedrive-subdir>]
    Writes %APPDATA%\refuge\config.toml: repos_dir, target_root, instance_id.
    Refuses a repos_dir inside target_root or inside any %OneDrive*% path.

refuge repo create <name>
refuge repo import <name> <path-to-existing-repo>
    Bare repo at <repos_dir>\<name>.git  (clone --mirror for import)
    git config refuge.repoid <uuid7>; gc.auto=0; maintenance.auto=false
    hooks/post-receive -> exec "<abs refuge.exe>" hook post-receive
    Prints:  git remote add refuge "<repos_dir>\<name>.git"

refuge hook post-receive          (cwd = bare repo, set by git)
    = refuge backup --repo-path . ; never exits non-zero in a way that hides push success
    (post-receive cannot fail the push anyway; it prints "refuge: backup failed: ..." if so)

refuge backup <name> | --repo-path <p>
    1. ref_state = for-each-ref + symbolic-ref HEAD ; hash
    2. if no refs -> write manifest with artifact=null, done
    3. git bundle create <staging>\<snapshot-id>.bundle --all
    4. git bundle verify ; bundle list-heads == ref_state (else re-run once)
    5. sha256, size
    6. copy to <target>\refuge\v1\repos\<repoid>\snapshots\<id>.bundle.partial ; rename
    7. write <id>.manifest.json  (LAST)
    8. print "protected <short-id> <n> refs <size>"

refuge status [<name>]
    For each repo: current ref-state hash vs newest manifest in target
    -> Protected | Pending (hash differs; run `refuge backup`) | Unprotected (no manifest)

refuge snapshots list [--target <root>] [<name|repoid>]
    Reads envelopes only. Marks `corrupt` if artifact missing or size mismatch.

refuge restore <name|repoid> [--snapshot <id>] [--as <name>] [--target <root>]
    newest valid manifest by default; sha256 check; bundle verify; clone --mirror;
    symbolic-ref HEAD; set refuge.repoid to the original; install hook; refuse to overwrite
    an existing <name>.git without --replace.
```

### Remote layout and manifest v1 (frozen from this iteration on)

```text
<target_root>\refuge\v1\repos\<repoid>\
    repo.json                       { "schema_version":1, "repo_id", "name", "created_at" }
    snapshots\<snapshot-id>.bundle
    snapshots\<snapshot-id>.manifest.json
```

```json
{
  "schema_version": 1,
  "repo_id": "0192...-uuid7",
  "repo_name": "beancount",
  "instance_id": "0192...-uuid7",
  "snapshot_id": "20260918T091530Z-g42-a1b2c3d4",
  "created_at": "2026-09-18T09:15:30Z",
  "generation": 42,
  "ref_state_hash": "sha256:...",
  "refs": { "HEAD": { "symref": "refs/heads/main" }, "refs/heads/main": "abc...", "refs/tags/v1": "def..." },
  "artifact": { "key": "snapshots/20260918T091530Z-g42-a1b2c3d4.bundle", "size": 123456,
                "checksum": "sha256:...", "format": "git-bundle", "format_version": 2 },
  "encryption": null,
  "refuge_version": "0.1.0"
}
```

`generation` in iteration 0 is `previous manifest generation + 1` read from the target (or 1).
Fields are only ever added; `encryption` becomes an object in iteration 3; `refs` may move to an
encrypted sidecar then, with `refs_sidecar` added to the envelope. Readers must ignore unknown
fields.

### Build order for today

| Step | What | Done when |
|---|---|---|
| 1 | `cargo new refuge`, deps: clap, serde, serde_json, toml, sha2, uuid(v7), time, anyhow | `refuge --version` |
| 2 | `git.rs`: run(), for_each_ref -> RefState, ref_state_hash, bundle_create/verify/list_heads, clone_mirror, symbolic_ref | unit test on a temp fixture repo with branch + annotated tag + notes |
| 3 | `config.rs` + `init` | config written, OneDrive-path guard works |
| 4 | `repo create/import` + hook script | `git push refuge main` from a work repo triggers the hook (hook body can be `echo` at this point) |
| 5 | `backup` + `manifest.rs` + fs target write (partial -> rename -> manifest last) | bundle + manifest appear in OneDrive folder after a push |
| 6 | `status`, `snapshots list` | status flips Pending -> Protected around a push |
| 7 | `restore` | wipe repos dir, restore from the OneDrive folder, clone it, branches and tags present, push again produces a new manifest |

Step 7 is the day's acceptance test and is PRD §21.4 minus the "new machine" part. Steps 1 to 5
are enough to start pushing real repositories; 6 and 7 can land the same evening.

Deliberately skipped in iteration 0: retries (a failed backup prints an error; the next push or a
manual `refuge backup` retries), disk-space check, audit log, `snapshots test`, `doctor`,
OneDrive account detection (the user picks the folder), server mode, encryption, daemon,
Linux support (builds, untested).

## Iteration 1: async and durable (next week)

Motivation: pushes to larger repos should not wait on bundling; failures should retry by
themselves.

- `refuge daemon run` with a reconcile loop (ref-state hash vs newest manifest, every 30 s and on
  spool wake-up) and one worker per repo.
- Hook becomes: write `<data>\spool\<repoid>.<nonce>` and exit. Backward compatible: a repo whose
  hook still calls `backup` inline keeps working.
- SQLite appears here, only for `jobs` and `events`. Protection state stays derived.
- Retry with bounded backoff; distinct `LocalDisk` error; staging cleanup on start.
- `refuge service install` (Task Scheduler at logon).
- Exit: PRD §21.3 with the target folder made read-only mid-run.

## Iteration 2: recovery confidence

- `refuge snapshots test <id>` (temp restore + fsck).
- `snapshots list` flags `unique_history` (older snapshot has refs unreachable from newest) and
  `foreign_writer` (manifest from another instance newer than ours).
- `refuge doctor`; `refuge repo remove`; `refuge snapshots purge --yes-delete-remote-backups`.
- Exit: PRD §21.5 (truncate newest bundle, restore from previous).

## Iteration 3: encryption (only if a repo needs it)

- `age` scheme from the implementation plan phase 5: per-target X25519 identity, passphrase-encrypted
  keyring file, encrypted artifact and `refs` sidecar, `encryption` object in the envelope.
- Exit: PRD §21.6.

## Iteration 4: server mode (Persona B)

- axum smart HTTP via `--stateless-rpc`, per-device tokens, loopback default, TLS via proxy.
- Backup target for a Linux server is a filesystem or NAS path (no OneDrive client there).
- Exit: PRD §21.2.

## Iteration 5: Graph adapter (optional)

Only if a real need appears for OneDrive from a Linux server, or for strict upload confirmation.
Same manifest and layout; the adapter just gains `stat` with `quickXorHash`.

## What "protected" means per iteration

| Iteration | `Protected` means |
|---|---|
| 0 to 4 with fs target | Verified bundle and manifest are in the target folder on local disk. Upload to OneDrive is the OneDrive client's responsibility. |
| 5 with Graph target | Upload confirmed by size and provider hash. |

This is a conscious deviation from PRD §11.7 step 5 for the sync-folder target and should be
recorded as a PRD amendment: "for filesystem targets, publication is complete when the manifest is
durably written to the target path".
