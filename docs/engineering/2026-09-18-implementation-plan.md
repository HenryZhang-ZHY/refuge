---
title: Refuge Implementation Plan
status: Reference (phase ordering superseded by 2026-09-18-iterative-delivery-path.md)
date: 2026-09-18
depends_on:
  - docs/product/2026-09-18-refuge-prd.md
  - docs/engineering/2026-09-18-prd-technical-review.md
---

# Refuge Implementation Plan

This is an aspirational long-term reference, not the current crate layout or
the next execution sequence. The implemented architecture remains a single
crate; introduce a daemon, queue, or SQLite only by calling the existing
application backup use cases and treating durable retry and reconciliation as
new behavior rather than a directory reorganization.

Language: Rust (stable, edition 2024). Runtime dependency: `git` 2.40 or newer on `PATH`.
First-release targets: Windows 11 x86_64 (local-first), Linux x86_64 (server mode).

Every phase ends with a runnable binary and an executable acceptance test. Phases 1 to 3
deliver the PRD's core invariant on a filesystem target; OneDrive-specific work, encryption, and
server mode are layered on afterwards so that the state machine is proven before the
provider-specific risk is added.

## 1. Architecture

```text
                 +------------------------------------------------------------+
  git client --> | bare repo (D:\refuge\repos\<repo-id>.git)                  |
  (file:// or   | hooks/post-receive -> `refuge hook post-receive`            |
   smart HTTP)   +------------------------------+-----------------------------+
                                                | spool/<repo-id>.<nonce> (wake-up file)
                                                v
 +------------------------------------------------------------------------------------+
 | refuge daemon                                                                       |
 |  reconcile loop: for-each-ref -> ref-state hash -> compare with db -> enqueue        |
 |  worker (1 per repo max): lock -> bundle create -> verify -> [encrypt] -> stage      |
 |                          -> target.put(artifact) -> target.put(manifest) -> confirm  |
 |  maintenance: git maintenance run under the same per-repo lock                      |
 |  state: SQLite (WAL)  repos | generations | jobs | snapshots | targets | events      |
 +--------------------------------------+---------------------------------------------+
                                        | StorageTarget trait
                 +----------------------+----------------------+
                 | fs (dir, sync folder, NAS)  | graph (later)  | s3 (later)
                 | + optional OneDrive probe   |                |
                 +-----------------------------+----------------+
```

Remote layout (identical on every provider):

```text
<root>/refuge/v1/
  instance/<instance-id>.json                # who has written here (for split-brain detection)
  repos/<repo-id>/
    repo.json                                # repo id, created_at; name only when not encrypted
    keyring/<keyring-version>.age            # only when encryption is enabled
    snapshots/<snapshot-id>.manifest.json    # public envelope; written LAST
    snapshots/<snapshot-id>.bundle[.age]     # artifact
    snapshots/<snapshot-id>.refs.age         # encrypted ref state, only when encryption is enabled
```

Publication rule for every adapter: artifact (and sidecars) are fully written and re-read for
checksum before the manifest is written. A manifest whose artifact is missing or whose checksum
mismatches is treated as absent by discovery and reported as `corrupt`.

## 2. Workspace layout

Keep the crate graph small. Split further only when compile times or ownership demand it.

```text
Cargo.toml                 # workspace
crates/
  refuge-core/             # domain: ids, RefState, Generation, Manifest, JobState, errors
  refuge-git/              # thin wrapper over `git` CLI: for_each_ref, bundle_{create,verify,
                           #   list_heads}, clone_mirror, fsck, maintenance, receive/upload-pack
  refuge-store/            # StorageTarget trait, fs adapter, OneDrive probe (cfg(windows))
  refuge-crypto/           # age keyring scheme, checksums, zeroizing passphrase handling
  refuge-daemon/           # SQLite schema, queue, reconcile loop, worker state machine
  refuge-http/             # axum smart-HTTP server + token auth (phase 6)
  refuge-cli/              # `refuge` binary: clap commands, output formatting, exit codes
tests/
  e2e/                     # integration tests that spawn real `git` and a temp fs target
docs/
```

Key dependencies: `clap`, `serde`/`serde_json`, `rusqlite` (bundled), `sha2`, `uuid` (v7),
`time`, `tracing` + `tracing-subscriber`, `thiserror`/`anyhow`, `fs2` (file locks), `notify`
(spool watch), `age`, `secrecy`/`zeroize`, `keyring`, `windows` (Cloud Files API and registry),
`tokio` + `axum` + `tower-http` (phase 6), `reqwest` + `oauth2` (phase 7).

Concurrency model: the daemon is synchronous threads. One reconcile thread, one worker thread
pool (default 2), one maintenance thread. `tokio` appears only inside `refuge-http`. This keeps
the state machine debuggable and keeps `Command` spawning simple.

## 3. Core types (refuge-core)

```rust
pub struct RepoId(Uuid);            // stable, v7
pub struct InstanceId(Uuid);
pub struct SnapshotId(String);      // "<utc-compact>-<generation>-<instance-short>-<rand>"

pub struct RefState { refs: BTreeMap<String, ObjectId>, head: HeadTarget }
pub struct RefStateHash([u8; 32]); // sha256 of canonical serialization

pub struct Generation { repo: RepoId, number: u64, ref_state_hash: RefStateHash, observed_at: UtcTs }

pub enum JobState { Queued, Creating, Verifying, Encrypting, Uploading, Publishing,
                    Confirming, Protected, RetryWait { until, attempt }, Failed { class },
                    Superseded }

pub enum ProtectionState { Protected, PublishedUnconfirmed, Pending(Reason),
                           Degraded(Reason), Unprotected, Unknown }

pub enum ErrorClass { Transient, Auth, Quota, LocalDisk, Verification, Corrupt, Permanent }
```

`ProtectionState` is **derived** at read time from `generations` and `snapshots`, never stored,
so a crash can never leave a stale `Protected` in the database (§22 invariant).

## 4. Phases

### Phase 0: skeleton and decisions (week 1)

- Cargo workspace, CI (fmt, clippy, test on Windows and Linux), `refuge --version`,
  `refuge doctor` (git version, data dir writable, OneDrive account detection stub).
- `refuge-git`: `for_each_ref`, `RefState` canonicalization and hash, `bundle_create`,
  `bundle_verify`, `bundle_list_heads`, `clone_mirror`, `fsck`. Integration tests against a
  real `git`, including notes refs, annotated tags, HEAD symref, and the empty-repo case.
- Write `docs/engineering/manifest-v1.md` (JSON schema) and `docs/engineering/state-machine.md`.

Exit: `cargo test` green on both OSes; a bundle round-trips a fixture repo with all ref types.

### Phase 1: local-first vertical slice (weeks 2 to 3)

- `refuge init --mode local` creates the data dir, SQLite schema, instance id.
- `refuge repo create <name>` / `refuge repo import <path>`: bare repo under `repos/<id>.git`,
  `gc.auto=0`, `maintenance.auto=false`, `post-receive` hook installed with the absolute binary
  path; prints the remote path to add.
- `refuge hook post-receive`: writes a spool file and exits. Must never fail the push; any error
  is logged and swallowed.
- `refuge target add fs <path>`: filesystem adapter with `put_atomic` (write to
  `<root>/.refuge-staging` on the same volume, fsync, rename), `list`, `get`, `stat`, `delete`.
  Cross-volume fallback: write with a `.partial` suffix then rename; manifest-last rule still
  guarantees §11.7.
- `refuge daemon run`: reconcile loop + single worker: lock, snapshot ref state, bundle, verify,
  list-heads equality check, sha256, put artifact, re-stat and compare size, put manifest,
  mark job `Protected`.
- `refuge repo status [repo]`: derived `ProtectionState`, last protected snapshot, pending job.
- `refuge repo backup <repo>` enqueues immediately.

Exit: PRD §21.1 steps 1 to 9 pass against a plain directory target (OneDrive confirmation is
phase 4). Also: push during an in-flight bundle produces a second snapshot, and status is never
`Protected` between them.

### Phase 2: durability and failure semantics (week 4)

- Retry: bounded exponential backoff with jitter, persisted `RetryWait { until, attempt }`;
  `ErrorClass::Auth` and `Quota` do not back off exponentially, they surface as `Degraded` and
  retry on a slow fixed schedule.
- Crash reconciliation on start: jobs in `Creating`/`Uploading`/`Publishing` are re-examined;
  staging files older than the job are removed; a manifest found remote without a local record
  is imported as a known snapshot (the remote is the source of truth for what was published).
- Disk-space check before bundle; distinct `LocalDisk` error.
- Temporary file budget: at most one staged artifact per repo; a failing job reuses or deletes
  its own staging file.
- Audit events table + JSONL log; `refuge events [--repo]`.
- Fault-injection test adapter (`fs` wrapped with "fail after N bytes" / "fail on manifest put"
  / "kill process here" hooks) driving PRD §20 scenarios as integration tests.

Exit: PRD §21.3 passes with the target directory made read-only or unmounted mid-run, the
daemon killed with SIGKILL/TerminateProcess at each state, and restarted.

### Phase 3: discovery, restore, corruption handling (week 5)

- `refuge snapshots list --target <t> [--repo]`: reads envelopes only; validates that each
  artifact exists and matches size; marks `corrupt`, `foreign_writer`, `unique_history`.
- `refuge snapshots test <snapshot>`: download to temp, checksum, `bundle verify`,
  `clone --mirror`, `fsck`, report, clean up. Non-destructive (§14.6).
- `refuge restore <snapshot> [--as <name>]`: steps 1 to 10 of PRD §14.4; refuses to overwrite an
  existing repo without `--replace`; restores `HEAD` symref; registers repo under the original
  `RepoId`; first backup after restore must succeed (this is the §21.4 step 9 test).
- `refuge target discover <t>`: lists repos and snapshots without any local database (run
  against an empty data dir in the test).

Exit: PRD §21.4 and §21.5 pass, including truncating the newest bundle and restoring from the
previous one.

### Phase 4: OneDrive sync-folder specifics and Windows service (week 6)

- OneDrive account detection from the registry; `refuge target add onedrive` resolves the sync
  root, shows `UserEmail` and Personal/Business, requires the user to confirm before enabling
  (§16.4). Refuses a target path that is inside a repo dir or vice versa.
- Windows Cloud Files probe: after manifest put, poll placeholder state until in-sync or until a
  configurable deadline (default 30 min), then `Protected`; otherwise stays
  `Pending(awaiting_sync)` and the reconcile loop keeps probing.
- Guard: refuse to create or import a repo whose path is under any known OneDrive root.
- Service install: `refuge service install` registers a Task Scheduler logon task (default) or a
  Windows service; `refuge service status`. Linux: emit a systemd unit.

Exit: PRD §21.1 fully passes on a Windows machine with a real OneDrive Business account; pausing
the OneDrive client keeps status at `Pending(awaiting_sync)`.

### Phase 5: optional encryption (week 7)

Scheme, using the `age` crate only (no bespoke container):

1. `refuge target encrypt enable <t>` generates a random age X25519 identity per target.
2. The identity is encrypted with the user passphrase (age scrypt recipient) and published as
   `keyring/1.age`. The passphrase is held in a `SecretString`, zeroized, and stored in the OS
   credential store only with `--remember`.
3. Artifacts and the ref-state sidecar are encrypted to the X25519 recipient. Encryption is
   streaming (age handles chunked AEAD), so large bundles are not held in memory.
4. Passphrase change (§13.3) publishes `keyring/2.age`; artifacts are untouched.
5. Wrong passphrase fails at keyring decryption (distinct from artifact corruption, which fails
   at artifact decryption or checksum), satisfying §13.4.
6. Manifest `encryption: { scheme: "age-x25519-v1", keyring_version: N }`.

Setup prints the "lost passphrase means unrecoverable backups" warning and requires typed
confirmation. Logs redact anything typed as `SecretString` by construction.

Exit: PRD §21.6 passes; `strings` on the remote artifact finds no plaintext from the fixture repo.

### Phase 6: server mode (weeks 8 to 9)

- `refuge init --mode server`; `refuge serve --bind 127.0.0.1:7788` (default loopback).
- `refuge-http`: smart HTTP v2 via `git upload-pack --stateless-rpc` and
  `git receive-pack --stateless-rpc`; request body streamed to the child's stdin, stdout streamed
  back; the child's post-receive hook enqueues exactly as in local-first mode.
- Auth: `refuge device add <name>` prints a one-time token; stored as Argon2id hash; Basic auth;
  `refuge device revoke <name>`. Reads require auth by default; `--allow-anonymous-read` opt-in.
- TLS: optional `--tls-cert/--tls-key`; startup warning when bound to a non-loopback address
  without TLS.
- Rate limiting and body size limits via `tower-http`.

Exit: PRD §21.2 passes with two clients on different machines through a Caddy or Tailscale
front; unauthenticated push returns 401 and leaves no job.

### Phase 7: Graph adapter and release engineering (weeks 10 to 11)

- `graph` adapter: device-code OAuth flow, `/me` and `/organization` shown before enabling,
  resumable upload sessions for artifacts over 4 MiB, `quickXorHash` and size verified after
  upload, throttling (429 with `Retry-After`) mapped to `Transient`. This is the first adapter
  that satisfies §11.7 step 5 strictly and the path for Persona B on Linux.
- Release: static binaries for Windows and Linux, checksums, `cargo install`, winget/scoop
  manifests, upgrade notes; `refuge doctor` becomes the first line of support.
- Metrics (§18): durations and sizes recorded per job in SQLite; `refuge stats`.

Exit: a Linux server-mode instance backs up to OneDrive Personal through Graph and is restored on
a fresh Windows machine through the sync folder, and vice versa.

## 5. Testing strategy

- **Unit**: ref-state canonicalization, snapshot id ordering, state transitions (property tests
  with `proptest` asserting that no sequence of events yields `Protected` without a confirmed
  snapshot whose ref-state hash equals the current one).
- **Integration**: every phase's exit criterion is a test under `tests/e2e` that spawns real
  `git`, uses a temp fs target, and (from phase 2) the fault-injection adapter.
- **Cross-platform**: CI matrix Windows + Linux for everything not `cfg(windows)`; the OneDrive
  probe and registry code are exercised manually on a Business account until a test tenant is
  available.
- **Recovery drill**: a scripted "wipe data dir, discover, restore, push, verify new backup"
  run is part of CI from phase 3 on and is the release gate.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Cloud Files API probe is unreliable or slow | Probe is best-effort with deadline; state model already has `PublishedUnconfirmed`; Graph adapter is the strict path. |
| Full bundles too large for frequent pushes | Debounce (default 15 s quiet period) before snapshotting; size and duration warnings; incremental bundles are a documented future item. |
| Hooks fail on Windows (`sh` missing, path with spaces) | Hook is a two-line script quoting the absolute exe path; reconcile loop makes hooks optional for correctness. |
| Company policy blocks unsigned binaries | Code signing in phase 7; document `cargo install` from source as the fallback. |
| Git version drift (`bundle` flags, protocol v2) | `refuge doctor` enforces the minimum; `refuge-git` tests pin behaviors. |
| Scope creep toward a Git UI | PRD §7.2 is the gate; anything not in §21 is a follow-on. |

## 7. Immediate next steps

1. Create the Cargo workspace and CI (phase 0).
2. Implement `refuge-git` with the fixture-based round-trip test, since every later phase depends
   on it and it surfaces Git edge cases (F8) earliest.
3. Write `manifest-v1.md` and get the envelope/sidecar split (F5) agreed before any manifest is
   published, because the schema becomes a compatibility contract with the first real backup.
