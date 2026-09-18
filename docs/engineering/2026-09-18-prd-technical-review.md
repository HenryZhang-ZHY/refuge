---
title: Refuge PRD Technical Review
status: Draft
date: 2026-09-18
reviews: docs/product/2026-09-18-refuge-prd.md
---

# Refuge PRD Technical Review

This document reviews the Refuge PRD from an implementation standpoint. It lists the
PRD's internal tensions and gaps, resolves the open implementation decisions in PRD §24,
and records the technology stack decision (Rust). The companion document
`2026-09-18-implementation-plan.md` turns these decisions into a phased plan.

Verdict: the PRD is implementable as written. Its core invariant (§22) is sharp and testable.
The main risks are not in the Git or backup logic; they are in (a) what "remotely published"
can mean on a OneDrive sync folder, (b) the fact that the two personas need different
OneDrive transports, and (c) a handful of under-specified terms ("generation", manifest
privacy under encryption, split-brain after restore). Each is addressed below.

## 1. Findings that change the design

### F1. §11.7 step 5 cannot be satisfied by a sync-folder adapter alone (blocking)

§11.7 says a backup is complete only when "the remotely stored artifact can be identified and
its size or checksum confirmed". §12.2 permits implementing OneDrive via the local synchronized
folder. Writing a file into that folder proves nothing about upload: the OneDrive client may be
paused, signed out, throttled, or out of quota.

Resolution:

- Model the sync folder as a generic **filesystem target** with an optional
  **upload-confirmation probe**. On Windows the probe uses the Cloud Files API placeholder state
  (`CfGetPlaceholderStateFromAttributeTag` and the `FILE_ATTRIBUTE_*` cloud flags) to determine
  whether the file is "in sync". Until the probe confirms, the repository state is `Pending` with
  reason `awaiting_sync`, never `Protected`.
- If the probe is unavailable (non-Windows, or the folder is not a OneDrive root), the target is
  configured as `confirmation = none` and the strongest state it can reach is a new sub-state
  `Published (unconfirmed)`. The CLI must render this distinctly from `Protected`.
- The Microsoft Graph adapter (phase 7) is the only OneDrive path that satisfies §11.7 step 5
  strictly (it returns size and `quickXorHash` on upload).

Recommendation: amend PRD §11.10 to add the `Published (unconfirmed)` state, or state explicitly
that sync-folder targets with no probe cannot reach `Protected`.

### F2. Persona A and Persona B require different OneDrive transports (scope)

Persona A runs on a company Windows laptop where a OneDrive client is present and where
registering a Graph application usually requires tenant-admin consent that IT will not grant.
Persona B runs Refuge on a Linux server or NAS, where no OneDrive client exists; only Graph
(or rclone) can reach OneDrive.

Resolution: the MVP OneDrive transport is the **sync folder (filesystem target + Windows probe)**,
which serves Persona A fully. Server mode ships in the MVP, but Persona B's OneDrive backup is
delivered by the Graph adapter in a follow-on phase. Until then server-mode users back up to a
filesystem or NAS target. This keeps "OneDrive is the first provider" true without making Graph
OAuth a launch dependency.

rclone is rejected as a primary transport: it is an external binary the user must install and
authenticate, it still requires OAuth, and it adds a second error model to translate.

### F3. "Generation" is undefined; define it as a ref-state hash

The PRD uses "generation N" throughout but never defines it. Define:

- **Ref state** of a repository = the sorted list of `(refname, object-id)` for all refs plus the
  symbolic target of `HEAD`.
- **Ref-state hash** = SHA-256 of the canonical serialization of the ref state.
- **Generation** = a monotonically increasing per-repository counter that the coordinator
  increments whenever it observes a ref-state hash different from the last recorded one.

A snapshot **covers** a generation iff the ref state recorded in its manifest equals that
generation's ref state. `Protected` means: the current ref-state hash equals the ref-state hash
of a snapshot whose publication (and confirmation, see F1) succeeded.

This definition makes correctness independent of hooks: a periodic or on-start **reconcile pass**
(`for-each-ref`, hash, compare) discovers any missed push. Hooks become a latency optimization,
which is exactly what §11.1 ("within 10 seconds") needs but §17.2 ("recompute protection state")
must not depend on.

Coalescing (§11.2) falls out naturally: a queued job is for "the repository", not for a generation;
when the worker starts it reads the current ref state and snapshots that. A snapshot of a later
state does not retroactively protect an earlier one; it supersedes it, which is what the
per-repository status needs.

### F4. Newer snapshots are not supersets of older ones

Force pushes and branch deletions mean the newest full bundle can lack commits that an older
bundle contains. This is consistent with §11.3 (the snapshot is the repository as accepted), but
it has two consequences the PRD should state:

- The default retention **must** be keep-all (already the PRD's stance in §11.12); a time-based
  policy silently destroys the only copy of force-pushed-away history.
- `refuge snapshots list` should flag snapshots whose ref state is not reachable from the newest
  snapshot, so the user knows which older snapshots hold unique history.

### F5. Manifests leak ref names when encryption is enabled

§11.8 puts ref names and object IDs in the manifest; §14.2 says discovery reads manifests. With
encryption on, branch names like `refs/heads/acquisition-of-acme` are plaintext in the remote.

Resolution: split the manifest into a **public envelope** (schema version, repo id, snapshot id,
timestamps, artifact key, size, checksum, encryption metadata, product version) and a
**ref-state section**. When encryption is off the ref-state section is inline. When encryption is
on it is stored as a separate encrypted sidecar (`<snapshot>.refs.age`), and the envelope carries
only the ref-state hash and ref count. Discovery still works from the envelope; showing branch
names in `snapshots list` requires the passphrase. The repository's human-readable name is also
moved into the encrypted part when encryption is on; the envelope shows the repo id.

### F6. Split-brain after clean-machine restore

After a restore, the new host registers the repository under the original repository id and
publishes new snapshots to the same remote path. If the original host is not actually dead (or
two people restore the same backup), two writers publish to one repository namespace.

Resolution:

- Every Refuge instance has an **instance id** (random, generated at `refuge init`). Snapshot ids
  embed it, and manifests record it.
- On each backup, before publishing, the worker lists remote manifests and warns (state
  `Degraded`, reason `foreign_writer`) if it finds a snapshot from another instance newer than
  its own last published snapshot. Publication still proceeds because remote artifacts are
  immutable and the user must decide which lineage is authoritative.

### F7. Consistent snapshots: disable Git's automatic maintenance on hosted repositories

`git bundle create --all` reads refs once and then walks objects. Because `receive-pack` writes
objects before updating refs, a concurrent push cannot make the walk miss an object. The only
process that can remove an object mid-walk is `git gc` or `repack -d`. Therefore:

- Set `gc.auto=0` and `maintenance.auto=false` on every hosted bare repository at creation or
  import.
- Refuge runs `git maintenance run` itself, under the same per-repository lock the snapshot
  worker takes. Pushes are never blocked by this lock; only maintenance and snapshotting
  serialize against each other.
- Read the ref state (F3) immediately before `bundle create` and verify after creation that the
  bundle's ref list (from `git bundle list-heads`) equals it. If a push landed in between, the
  bundle is still coherent but represents the pre-push generation; the worker records it as such
  and immediately requeues.

### F8. Edge cases `git bundle` forces on us

- **Empty repository**: `git bundle create` refuses to create an empty bundle. A repository with
  zero refs is trivially protected; publish a manifest with `artifact: null` and report
  `Protected (empty)`.
- **HEAD**: `--all` includes `HEAD` as a resolved ref, not as a symref. Record the symref target
  in the manifest and restore it with `symbolic-ref` after `clone --mirror`.
- **Refs to non-commit objects** (`refs/notes`, refs pointing to blobs) are supported by bundles;
  verify with an integration test rather than assuming.
- **Not covered by bundles**: config, hooks, reflogs, `info/`, worktree metadata. None are needed
  for recovery; Refuge regenerates config and hooks on restore. Document this in the manifest
  `format` description.

### F9. Server-mode transport and authentication

The PRD leaves SSH vs smart HTTP open. Decision: **smart HTTP first**, implemented in-process by
piping request bodies to `git upload-pack --stateless-rpc` and `git receive-pack --stateless-rpc`.
Authentication is HTTP Basic with per-device tokens (random, stored hashed with Argon2id,
revocable individually, satisfying §16.2). Reasons: Git clients handle it with the standard
credential helper; the server is a few hundred lines on top of `axum`; no host-key management.

Constraint that must be documented loudly: Basic auth over plaintext HTTP exposes the token.
Refuge binds to loopback by default in both modes; server mode requires either a TLS-terminating
reverse proxy (Caddy) or an overlay network (Tailscale, WireGuard). Refuge itself terminates TLS
only if a certificate path is configured; it never obtains public certificates.

SSH (via `russh`) is a later addition; it is preferable for Persona B ergonomically but roughly
triples the transport code.

### F10. Local-first transport: filesystem remote plus hooks plus reconcile

In local-first mode the Git remote is the bare repository path (`D:\refuge\repos\<id>.git`).
Pushes go straight through the Git client's own `receive-pack`; no Refuge process is on the
path, so a push cannot be blocked by Refuge being down (§5.4 strengthened). The `post-receive`
hook runs `refuge hook post-receive`, which appends a durable wake-up record to the spool
directory and returns immediately. The daemon wakes on a filesystem watch of the spool directory
or on a short poll, and the reconcile pass (F3) covers the case where the hook did not run.

A loopback smart-HTTP listener is also available in local-first mode for users who prefer a URL
remote, but it is off by default (§9.1).

## 2. Smaller gaps and clarifications

- **Deletion**: `refuge repo remove` unregisters and optionally deletes the bare repo; remote
  deletion is `refuge snapshots purge --repo <id> --yes-delete-remote-backups` and is the only
  destructive remote command in the MVP.
- **Disk exhaustion (§17.4)**: before `bundle create`, compare free space on the staging volume
  against the repository's on-disk size times 1.2; fail early with a distinct error class.
- **Path constraints in OneDrive**: names are limited to 400 characters and forbid
  `" * : < > ? / \ |`. Remote paths therefore use repository ids and snapshot ids only, never
  repository names.
- **Time**: snapshot ids embed UTC time for readability; "newest" is decided by manifest
  `created_at` with `generation` as tiebreaker, never by file mtime. Clock skew on the restore
  machine does not matter because manifests are compared, not files.
- **Account identification (§16.4)**: on Windows read
  `HKCU\Software\Microsoft\OneDrive\Accounts\{Personal,Business1,...}` for `UserEmail`,
  `UserFolder`, `ServiceEndpointUri`; match the configured target path to `UserFolder` and show
  the email and whether the account is Personal or Business before enabling the target.
- **Audit log (§16.5)**: structured JSON lines in a rotating file plus the same events in the
  SQLite `events` table; `refuge events` renders them.
- **Import (§10.3)**: `git clone --mirror` into the repository directory preserves all refs and
  object ids. For an existing bare repository, moving or copying the directory is allowed; Refuge
  then rewrites config and installs hooks.

## 3. Resolution of PRD §24 open decisions

| Decision | Resolution |
|---|---|
| Git transport per mode | Local-first: filesystem remote, loopback HTTP optional. Server: smart HTTP with token auth; SSH later. |
| Supported OS for first release | Windows 11 (Persona A). Linux x86_64 for server mode. macOS builds but the OneDrive probe is untested. |
| OneDrive integration | Sync folder via filesystem target + Windows Cloud Files probe (MVP). Graph adapter in phase 7. rclone rejected. |
| Operational state and durable queue | SQLite (WAL) via `rusqlite` with the bundled feature; queue is a table, hook wake-ups are files in a spool directory. |
| Consistent snapshot strategy | F7: auto-gc disabled, Refuge-owned maintenance under a per-repo lock, ref state checked before and after bundle. |
| Manifest format and versioning | JSON, integer `schema_version`, additive changes only; public envelope + optional encrypted ref sidecar (F5). |
| Encryption library | `age` format via the `age` crate. Per-target random X25519 identity; identity encrypted with the passphrase (scrypt) as a versioned keyring file. See plan phase 5. |
| Credential storage | `keyring` crate (Windows Credential Manager, macOS Keychain, Secret Service). Passphrase never persisted unless the user opts into the OS store. |
| Packaging and service | Single static binary. Windows: Task Scheduler at logon by default, optional Windows service. Linux: systemd unit. winget/scoop and `cargo install` later. |
| Default retention | Keep all. `refuge target usage` shows consumption. Retention policies are opt-in and never delete the newest verified snapshot. |
| Size limits and warnings | Warn at 1 GiB bundle size and when snapshot creation exceeds 5 minutes; hard limit configurable, default none. |

## 4. Technology stack: Rust

Rust is a good fit and is adopted. Reasons that matter for this product specifically:

- One statically linked binary that is the CLI, the hook, the daemon, and the HTTP server.
  Installation on a locked-down company laptop is copying one file.
- The daemon is a long-running state machine that must never corrupt its queue; Rust's type system
  lets the job state machine (§19) be encoded so invalid transitions do not compile.
- Mature crates for every dependency: `tokio`, `axum`, `clap`, `rusqlite`, `serde`, `sha2`,
  `age`, `keyring`, `tracing`, `windows` (Cloud Files API), `reqwest` + `oauth2` (Graph, later).

Where Rust does **not** help and we should not pretend otherwise:

- **Server-side Git in pure Rust is not production-ready.** `gitoxide` has no `receive-pack` and
  its `upload-pack` is incomplete. Refuge shells out to the system `git` for `bundle create`,
  `bundle verify`, `receive-pack`, `upload-pack`, `fsck`, `clone --mirror`, and maintenance.
  `git` 2.40 or newer is a hard runtime dependency, and `refuge doctor` checks for it. `gix` may
  be used read-only (ref enumeration) where it removes a process spawn, but `git for-each-ref` is
  acceptable for the MVP.
- Development speed will be slower than Go or Python for the first vertical slice. Mitigation:
  keep the crate graph small (see plan §2) and avoid async except in the HTTP server and the
  Graph adapter; the backup worker is a plain thread driving `std::process::Command`.

Go was the credible alternative (same single-binary story, faster iteration, `go-git` equally
lacking on the server side). It was not chosen because passphrase and token handling benefits
from Rust's ownership rules (zeroization, no accidental copies into logs), and because the author
prefers Rust.

## 5. Suggested PRD amendments

Small text changes that would remove ambiguity for future readers:

1. §11.10: add `Published (unconfirmed)` or define that sync-folder targets without an upload
   probe cannot report `Protected`. (F1)
2. §19: adopt the ref-state definition of generation. (F3)
3. §11.8: allow the ref list to live in an encrypted sidecar when encryption is enabled. (F5)
4. §10.1: add "instance id" to repository and snapshot identity and describe the foreign-writer
   warning. (F6)
5. §11.12: state explicitly that newer snapshots may not be supersets and that keep-all is the
   default for that reason. (F4)
6. §7.1: state that MVP server mode ships with filesystem or NAS backup targets and that OneDrive
   for Linux servers arrives with the Graph adapter. (F2)
