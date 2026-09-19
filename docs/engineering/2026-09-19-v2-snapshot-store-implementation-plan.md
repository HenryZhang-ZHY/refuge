---
title: Refuge v2 Snapshot Store — From-Scratch Implementation Plan
status: Proposed (supersedes 2026-09-19-incremental-backup-redesign.md and manifest-v1.md)
date: 2026-09-19
audience: coding agents implementing the rewrite
outline: |
  - L1 Principle: 100% reliability first, then the simplest architecture that achieves it
    - L2 No backward compatibility, no migration: nothing has shipped
  - L1 One immutable, content-addressed, append-only store per repository
    - L2 Git objects travel in a linear chain of bundles: one checkpoint plus deltas
    - L2 Git LFS objects are published once, addressed by their SHA-256 OID
    - L2 A tiny manifest, written last, is the only thing that makes a recovery point exist
  - L1 Exact algorithms, module map, schemas, Git commands, tests, and milestones
---

# Refuge v2 Snapshot Store — From-Scratch Implementation Plan

## 0. How to use this document

This document is the specification for rewriting Refuge's snapshot store: the on-target
layout, the manifest, backup, discovery/status, and restore. It is written so that a coding
agent can implement it milestone by milestone without further design decisions.

Rules for implementers:

1. Where this document says MUST, do exactly that. Where it is silent, choose the simplest
   correct option and add a test.
2. Do not add configuration knobs, caches, databases, or abstractions that this document does
   not name. Simplicity is a requirement, not a preference.
3. Every milestone ends with `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets
   -- -D warnings`, and `cargo test --locked` green on Linux and Windows.
4. Follow the existing code conventions (`anyhow` with `.context(...)`, `tests/support` for
   isolated subprocess environments, no global Git configuration leakage).
5. Nothing published by Refuge 0.x has to be read. Delete old code and old documents instead
   of keeping them alive.

Sections 1–3 explain the design and its reasoning. Sections 4–12 are the normative
specification. Section 13 is the milestone plan. Section 14 is the acceptance checklist.

## 1. Problem statement (recap)

A real Obsidian repository has ~100 MB of Git data and ~600 MB of Git LFS data and is pushed
three to five times a day. Refuge 0.x publishes a complete bundle and a complete LFS tar
archive per push: ~700 MB per push, 63–105 GB per month. The failure is structural: storage
grows with `repository size × push count` instead of with new content.

## 2. Design principle and what it changes versus the previous proposal

The guiding principle is: **guarantee correctness with the fewest moving parts.** Concretely:

- One storage format, one publication protocol, one restore algorithm. No dual-format readers.
- Every file in the target is immutable and either content-addressed or uniquely named.
- Verification is deep where it is cheap relative to the data movement that just happened
  (publish time, restore time, explicit `verify`), and shallow (presence + size) where it must
  run constantly (status, catalog listing). This rule is applied uniformly.
- Correctness never depends on cloud-client behavior, file mtimes, or clocks.

The previous proposal (`2026-09-19-incremental-backup-redesign.md`) reached the right storage
model but carried costs that this plan removes:

| Previous proposal | This plan |
|---|---|
| Keep v1 full bundles and LFS tars readable; deliver LFS first, bundles second | One `v2` format delivered at once; v1 code and docs are deleted |
| Four checkpoint thresholds (age, count, bytes ratio, manual) | One amortized byte rule plus one count bound plus `--checkpoint` |
| Re-verify the checksum of every existing LFS object in the target before reuse (re-reads 600 MB per push) | Size check on reuse; deep verification at publish, restore, and `snapshots verify` |
| Separate "object-set index" phase | The set file is part of the format from day one |
| Retention, GC, coalescing, and observability as later phases | `usage` and `verify` in the core; `prune` as one small, optional milestone; no coalescing |
| Silent about existing implementation defects | Fixes them (see §2.1) |

### 2.1 Defects in the current implementation that the rewrite removes

These are not format issues, but the rewrite touches every one of them, so they are listed
here so nobody re-implements them:

1. **Staging and locks live inside the sync folder.** `<target>/.refuge-staging/` holds lock
   files and bundle staging. OneDrive syncs dot-directories, so temporary bundles and lock
   files are uploaded. v2 stages in the local data directory and uses `.tmp` partial files
   (which the OneDrive client does not upload) inside the target.
2. **Three Git subprocesses per blob during LFS pointer scanning** (`cat-file -t`, `-s`,
   `blob`). A 100 MB repository has tens of thousands of blobs. v2 uses `cat-file --batch-check`
   and `cat-file --batch` (two processes total).
3. **Manual backup when already protected publishes a redundant full snapshot.** v2 backup
   is idempotent: same ref state, healthy newest snapshot → no-op.
4. **`bundle list-heads == refs` consistency check does not hold for incremental bundles**
   (Git omits refs whose tip is a prerequisite). v2 checks ref state before and after bundle
   creation and requires bundle heads ⊆ ref state.
5. **Restore via `git clone --mirror <bundle>`** cannot replay a chain. v2 uses
   `git bundle unbundle` (objects only) plus one `git update-ref --stdin` transaction that sets
   refs exactly from the manifest.
6. **Windows prints a durability warning on every backup** because directory `fsync` is
   reported as unsupported. v2 skips directory sync silently on Windows (NTFS journals
   metadata; file `sync_all` is sufficient).
7. **`repo.json` is redundant** (every manifest carries `repo_name`). Removed.
8. **Snapshot id collision after a crash.** A crashed attempt leaves `git/<id>.bundle`; the
   retry computes the same generation and can produce the same id, and publication then fails
   with "would overwrite". v2 treats a bundle without a manifest as an orphan and removes it.
9. **Generation is derived from valid manifests only**, so an unreadable manifest's generation
   can be reused. v2 also parses `g<N>` from every filename in `snapshots/`.

## 3. Storage model

### 3.1 Layout

```text
<target>/refuge/v2/repos/<repo-id>/
  git/<snapshot-id>.bundle                 Git bundle: full checkpoint or incremental delta
  lfs/objects/<oid[0:2]>/<oid>             Git LFS object content, addressed by SHA-256
  lfs/sets/<sha256-hex>.txt                canonical "<oid> <size>\n" list, addressed by its own SHA-256
  snapshots/<snapshot-id>.json             manifest; written last; the only publication marker
```

- `repo-id` is the UUID v7 stored in the bare repository as `refuge.repoid` (unchanged).
- `snapshot-id` = `<YYYYMMDDTHHMMSSZ>-g<generation>-<instance-short>` (unchanged format).
  `instance-short` is the first 8 hex characters of the simple UUID form of `instance_id`.
- Every path under `<repo-id>/` is immutable once its final name exists. Nothing is ever
  modified in place. `git/*.bundle` and `snapshots/*.json` are uniquely named; `lfs/**` files
  are content-addressed, so the same content always has the same path.
- LFS objects use one level of sharding (`<oid[0:2]>/`) to keep the OneDrive item count low.
- There is no per-target or per-repository index file. The set of `snapshots/*.json` files is
  the catalog.

### 3.2 Snapshot semantics

A **snapshot** is one manifest. It records the complete ref state of the repository at one
moment plus everything needed to rebuild the object database and LFS store for that state:

- `git.parent`: the snapshot id whose object closure this snapshot builds on, or `null`.
- `git.bundle`: the bundle published by this snapshot, or `null` if no new Git objects were
  needed (ref rewind, ref deletion, new ref on an existing commit, or empty repository).
- `lfs.set`: the content-addressed list of every LFS object reachable from the recorded refs,
  or `null` if there are none.

A snapshot with `git.parent == null` is a **checkpoint**: its bundle contains every object
reachable from its refs. A snapshot with a parent is a **delta**: its bundle contains exactly
the objects reachable from its refs that are not reachable from the parent's refs. The
**chain** of a snapshot is the list `[checkpoint, delta₁, …, deltaₙ = snapshot]` obtained by
following `git.parent` to `null`. Chains are linear: each snapshot has at most one parent, and
the parent always has a lower generation.

Restoring a snapshot means: unbundle every bundle in its chain in order, set refs exactly as
the manifest says, copy every LFS object in its set, and verify everything.

### 3.3 Invariants

1. **Manifest-last.** A snapshot exists iff `snapshots/<id>.json` exists. All files it
   references are fully written, flushed, verified, and renamed into place before the manifest
   is written. A reader that sees the manifest sees all of its dependencies (on the same
   filesystem).
2. **Never overwrite.** No final path is ever written twice. Content-addressed paths may be
   *reused* (skipped) when they already exist with the expected size. A content-addressed
   file with the wrong size is a damaged target; backup fails loudly and never replaces it.
3. **A dependency is either valid or the snapshot is corrupt.** A snapshot whose chain has a
   missing/invalid parent manifest, a missing/wrong-size bundle, a missing/wrong-size/wrong-hash
   set file, or a missing/wrong-size LFS object is `corrupt` and is never selected for default
   restore and never counts for `Protected`.
4. **Protected means restorable from the target.** `Protected` requires: the newest snapshot
   is valid (invariant 3) and its `ref_state_hash` equals the live repository's.
5. **Newest is decided by `(generation, snapshot_id)` within one repository**, never by
   filesystem timestamps or `created_at`.
6. **Deep verification at every data movement.** Every byte written to the target is hashed
   from the source stream, flushed, re-read, and hashed again before rename. Every byte copied
   out of the target during restore is hashed and compared to the manifest before use.

### 3.4 Storage behavior for the motivating repository

- First backup: ~100 MB checkpoint + ~600 MB LFS objects + one set file + one manifest.
- A Git-only push: one delta bundle proportional to the new objects (typically KB to a few MB),
  the same set file (already present → skipped), one manifest of a few KB. Zero LFS bytes.
- A push adding one 5 MB image: as above plus one 5 MB LFS object plus one new set file.
- Checkpoint policy (§7.4): a new checkpoint when the chain's delta bytes reach the checkpoint's
  size or the chain reaches 256 snapshots. Restore cost is therefore bounded by ~2× repository
  size and ≤ 257 bundle files.

## 4. Manifest v2 (normative)

### 4.1 Schema

```json
{
  "schema_version": 2,
  "repo_id": "0192d23c-0000-7000-8000-000000000000",
  "repo_name": "notes",
  "instance_id": "0192d23d-0000-7000-8000-000000000000",
  "snapshot_id": "20260919T103000Z-g42-a1b2c3d4",
  "created_at": "2026-09-19T10:30:00Z",
  "generation": 42,
  "refuge_version": "1.0.0",
  "ref_state_hash": "sha256:…",
  "refs": {
    "HEAD": { "symref": "refs/heads/main" },
    "refs/heads/main": "0123456789abcdef0123456789abcdef01234567",
    "refs/tags/v1": "…"
  },
  "git": {
    "parent": "20260919T090000Z-g41-a1b2c3d4",
    "bundle": { "key": "git/20260919T103000Z-g42-a1b2c3d4.bundle", "size": 204800, "checksum": "sha256:…" }
  },
  "lfs": {
    "set": { "key": "lfs/sets/3f1a….txt", "size": 451200, "checksum": "sha256:3f1a…" },
    "count": 5012,
    "size": 629145600
  }
}
```

Rust types (module `manifest`):

```rust
pub struct Manifest {
    pub schema_version: u32,                 // MUST be 2
    pub repo_id: Uuid,
    pub repo_name: String,
    pub instance_id: Uuid,
    pub snapshot_id: String,
    pub created_at: String,                  // RFC 3339, UTC, informational only
    pub generation: u64,                     // ≥ 1; ordering key
    pub refuge_version: String,
    pub ref_state_hash: String,              // "sha256:<64 hex>", refuge-ref-state-v1 domain (unchanged)
    pub refs: BTreeMap<String, ManifestRef>, // unchanged representation
    pub git: GitSection,
    pub lfs: Option<LfsSection>,
}
pub struct GitSection { pub parent: Option<String>, pub bundle: Option<Artifact> }
pub struct Artifact { pub key: String, pub size: u64, pub checksum: String }
pub struct LfsSection { pub set: Artifact, pub count: u64, pub size: u64 }
```

There is no `encryption` field and no `format`/`format_version` fields. `schema_version`
covers the whole format. Readers ignore unknown fields (serde default behavior); writers never
emit fields not listed here.

### 4.2 Manifest validation (`manifest::validate(path, &Manifest)`)

Every rule below is a hard error classified as `Corrupt`, except schema version which is
`Unsupported`:

1. `schema_version == 2`.
2. `snapshot_id` is non-empty, consists of `[A-Za-z0-9_-]`, and `path.file_name() ==
   "<snapshot_id>.json"`.
3. `generation >= 1`.
4. `refs`: `HEAD`, if present, is `{symref}` with a valid ref name; every other entry is a
   valid ref name (`refs/…`, no `..`, no `\`, `:`, space, control chars) mapping to a 40- or
   64-hex object id. The recomputed ref-state hash equals `ref_state_hash`.
5. `git.parent`, if present: same charset as `snapshot_id`, and `!= snapshot_id`.
6. `git.bundle`, if present: `key == "git/<snapshot_id>.bundle"`, `checksum` is `sha256:<64 hex>`.
7. If `git.parent == null` and `refs` contains at least one object ref, then `git.bundle` MUST
   be present (a checkpoint of a non-empty repository has a bundle).
8. If `refs` contains no object refs, then `git.bundle` MUST be `null` and `lfs` MUST be `null`.
9. `lfs`, if present: `count >= 1`; `set.key == "lfs/sets/<hex>.txt"` where `<hex>` is exactly
   the 64-hex digest in `set.checksum` (`"sha256:<hex>"`).
10. The manifest file is at most 8 MiB (read with a bounded reader).

Chain-level validation (needs the catalog) is in §8.3.

### 4.3 LFS set file

- Content: for every `(oid, size)` sorted by `oid` ascending: `"<oid> <size>\n"`. `oid` is 64
  lowercase hex. No header, no blank lines, no trailing whitespace.
- Key: `lfs/sets/<sha256-hex-of-content>.txt`. `Artifact { key, size: content.len(), checksum:
  "sha256:<same hex>" }`.
- Parsing is strict: any deviation (unsorted, duplicate, uppercase, bad size) is `Corrupt`.
- Empty sets are never written; `lfs` is `null` instead.

## 5. Module map

```text
src/
  main.rs            CLI (clap). Adjusted commands, see §11.
  lib.rs
  config.rs          unchanged
  repo.rs            unchanged, except: lock files move to locks_dir() (see layout.rs)
  application.rs     unchanged
  backup_queue.rs    unchanged (calls backup::backup_path)
  server.rs          unchanged except printing of BackupOutcome
  git.rs             extended (§6.1); existing helpers kept
  lfs.rs             rewritten (§6.2): pointer scan, set type, local verification; NO tar
  layout.rs          NEW (§6.3): all path derivations for target and local data dirs
  store.rs           REPLACES storage.rs (§6.4): verified publish/copy primitives
  manifest.rs        rewritten (§4)
  catalog.rs         REPLACES discovery.rs (§8): read manifests, order, chain, health, status
  backup.rs          rewritten (§7)
  restore.rs         rewritten (§9); keeps the --replace recovery-record logic
  verify.rs          NEW (§10): `snapshots verify` and `snapshots usage`
  prune.rs           NEW, optional milestone (§12)
```

Delete: `src/storage.rs`, `src/discovery.rs`, the `tar` dependency, `lfs::create_archive`,
`lfs::extract_archive`, `docs/engineering/manifest-v1.md`.

Unchanged product surfaces: local-first synchronous hook backup, server-mode queue and worker,
Smart HTTP and LFS endpoints, web UI, `repo create/import/clone/connect/list/view`.

## 6. Building blocks

### 6.1 `git.rs` additions

All commands go through the existing `command(repo)` builder (strips `GIT_DIR` and friends,
adds `-c safe.bareRepository=all -C <repo>`). Any command that feeds stdin MUST write the input
to a temporary file in the local staging directory and pass it as `Stdio::from(File)`; never
write to a pipe while also reading stdout (deadlock). Outputs are captured with `.output()`.

```rust
pub enum BatchCheck { Found { oid: String, kind: String, size: u64 }, Missing }

/// `git cat-file --batch-check` over the given object names (one per line). Returns one
/// entry per input line in input order. Input may include revision syntax such as
/// "<oid>^{commit}"; a name that does not resolve yields Missing.
pub fn batch_check(repo: &Path, names: &[String]) -> Result<Vec<BatchCheck>>;

/// `git cat-file --batch` over the given blob oids. Returns (oid, bytes) in input order.
/// Errors if any object is missing or is not a blob.
pub fn batch_blob_contents(repo: &Path, oids: &[String]) -> Result<Vec<(String, Vec<u8>)>>;

/// `git rev-list --objects --all` → all reachable object ids (first token per line). Unchanged.
pub fn reachable_objects(repo: &Path) -> Result<Vec<String>>;

/// `git rev-list --objects --all ^<x> ^<y> …` — true iff stdout is non-empty.
pub fn has_objects_outside(repo: &Path, exclusions: &[String]) -> Result<bool>;

/// `git bundle create <dest> --all ^<x> ^<y> …` (exclusions may be empty → full bundle).
pub fn bundle_create(repo: &Path, dest: &Path, exclusions: &[String]) -> Result<()>;

/// `git bundle verify <bundle>` run inside `repo` (checks prerequisites exist there). Unchanged.
pub fn bundle_verify(repo: &Path, bundle: &Path) -> Result<()>;

/// `git bundle list-heads <bundle>` → refs (HEAD excluded). Unchanged.
pub fn bundle_list_heads(bundle: &Path) -> Result<BTreeMap<String, String>>;

/// `git bundle unbundle <bundle>` inside `repo`: verifies prerequisites, writes objects,
/// does not touch refs.
pub fn bundle_unbundle(repo: &Path, bundle: &Path) -> Result<()>;

/// `git update-ref --stdin` with one `create <ref> <oid>` line per entry, in one transaction.
/// Fails if any ref exists or any object is missing.
pub fn create_refs(repo: &Path, refs: &BTreeMap<String, String>) -> Result<()>;
```

Constraints:

- `MAX_EXCLUSIONS = 512`. Callers MUST NOT pass more (Windows command-line limit is 32,767
  characters; 512 × 42 ≈ 21.5 KB). Backup falls back to a checkpoint above this (§7.3).
- Do not use `git bundle create --stdin` (its child `rev-list` competes for stdin).
- Do not use `git fetch` from bundles for restore; `unbundle` + `create_refs` is the only path.

### 6.2 `lfs.rs`

```rust
/// oid → size. Canonical ordering is by oid because BTreeMap.
pub type LfsSet = BTreeMap<String, u64>;

/// Scan all reachable objects for Git LFS pointers:
///   1. reachable_objects(repo)
///   2. batch_check(...) → keep kind == "blob" && size <= 1024
///   3. batch_blob_contents(...) → parse_pointer on each
/// Conflicting sizes for one oid → error. Returns the exact set for the current refs.
pub fn required_set(repo: &Path) -> Result<LfsSet>;

/// Serialize per §4.3. Returns (bytes, Artifact).
pub fn encode_set(set: &LfsSet) -> (Vec<u8>, manifest::Artifact);
/// Strict parser per §4.3. Verifies sha256(bytes) == expected digest from the key.
pub fn decode_set(bytes: &[u8], expected_checksum: &str) -> Result<LfsSet>;

/// <repo>/lfs/objects/<oid[0:2]>/<oid[2:4]>/<oid>  (git-lfs's own layout, used in hosted repos)
pub fn local_object_path(repo: &Path, oid: &str) -> PathBuf;
```

`parse_pointer` is kept as is (version line, `oid sha256:`, `size`). The 1024-byte candidate
limit is kept.

### 6.3 `layout.rs` (pure functions; unit-tested; the only place paths are derived)

```rust
pub const TARGET_ROOT: &str = "refuge/v2/repos";

pub struct RepoLayout { root: PathBuf }              // <target>/refuge/v2/repos/<repo-id>
impl RepoLayout {
    pub fn new(target: &Path, repo_id: Uuid) -> Self;
    pub fn root(&self) -> &Path;
    pub fn snapshots_dir(&self) -> PathBuf;             // root/snapshots
    pub fn manifest_path(&self, snapshot_id: &str) -> PathBuf;
    pub fn bundle_key(snapshot_id: &str) -> String;      // "git/<id>.bundle"
    pub fn lfs_object_key(oid: &str) -> String;          // "lfs/objects/<oid[0:2]>/<oid>"
    pub fn lfs_set_key(hex: &str) -> String;             // "lfs/sets/<hex>.txt"
    pub fn resolve(&self, key: &str) -> Result<PathBuf>; // root.join(key) after validating the key:
        // relative, only Normal components, exactly one of the three shapes above.
}

// Local (never synced) directories, derived from Config::repos_dir:
pub fn locks_dir(config) -> PathBuf;     // <repos_dir>/.refuge-locks
pub fn staging_dir(config) -> PathBuf;   // <repos_dir>/.refuge-staging
pub fn snapshot_id(now: OffsetDateTime, generation: u64, instance: Uuid) -> String;
pub fn generation_from_file_name(name: &str) -> Option<u64>;   // parses "-g<N>-" from "<id>.json"
```

`repo::list` already ignores directory names that do not end in `.git`, so `.refuge-locks` and
`.refuge-staging` are invisible to it. `repo.rs` MUST switch its name/identity lock files to
`locks_dir`.

### 6.4 `store.rs` (verified I/O against the target)

```rust
pub struct Store { layout: RepoLayout }

pub struct Written { pub bytes_written: u64, pub warnings: Vec<String> }

impl Store {
    /// Size of a regular file at `key`, or None if absent. Uses symlink_metadata; a
    /// non-regular file (symlink, dir) is an error, not None.
    pub fn stat(&self, key: &str) -> Result<Option<u64>>;

    /// Read a small file (≤ max) fully.
    pub fn read(&self, key: &str, max: u64) -> Result<Vec<u8>>;

    /// Publish `source` at `key`. Steps, in order:
    ///  1. create the destination directory;
    ///  2. create `.refuge-<random>.tmp` in that directory (tempfile::Builder prefix
    ///     ".refuge-", suffix ".tmp");
    ///  3. stream-copy source → tmp while hashing the source stream; compare hash and size
    ///     with `expected` (mismatch → error, tmp deleted);
    ///  4. `sync_all()` the tmp file;
    ///  5. re-open tmp, hash it again, compare with `expected` (mismatch → error);
    ///  6. `persist_noclobber(final)`; an existing final path is an error;
    ///  7. on Unix, fsync the directory; failure → warning. On Windows, skip silently.
    pub fn publish_file(&self, source: &Path, key: &str, expected: &Artifact) -> Result<Written>;

    /// Same protocol for an in-memory buffer (set files, manifests).
    pub fn publish_bytes(&self, bytes: &[u8], key: &str) -> Result<Written>;

    /// Restore side: stream-copy target/<key> → dest while hashing; compare with expected;
    /// mismatch → Err(Mismatch), NotFound/UnexpectedEof → Err(Missing), other I/O → Err(Io).
    pub fn copy_out(&self, key: &str, dest: &Path, expected: &Artifact) -> Result<(), CopyError>;

    /// Remove the final path at `key` if it exists (used only for orphan bundles, §7.6).
    pub fn remove(&self, key: &str) -> Result<()>;

    /// Delete `.refuge-*.tmp` files older than 24 h anywhere under the repository root.
    pub fn sweep_partials(&self) -> Result<Vec<String>>;   // returns warnings
}

pub fn sha256_file(path: &Path) -> Result<(String /* "sha256:…" */, u64)>;
```

Why `.tmp`: Microsoft documents that the OneDrive sync client does not upload files with the
`.tmp` extension. Partial files therefore never reach the cloud, and the final `rename` makes a
complete file appear atomically.

Placeholders: OneDrive Files On-Demand placeholders are reparse points with the cloud tag; Rust
reports them as regular files with the full size, and reading hydrates them. `stat` therefore
works for shallow health, and `copy_out` triggers hydration during restore. No special handling.

## 7. Backup (`backup.rs`)

```rust
pub struct BackupOptions { pub checkpoint: bool }

pub enum BackupOutcome {
    AlreadyProtected { snapshot_id: String },
    Published { manifest: Manifest, kind: SnapshotKind,
                git_bytes_written: u64, lfs_bytes_written: u64, lfs_objects_written: u64,
                warnings: Vec<String> },
}
pub enum SnapshotKind { Empty, Checkpoint, Delta, RefsOnly }

pub fn backup_path(config: &Config, repo: &Path, options: BackupOptions) -> Result<BackupOutcome>;
```

### 7.1 Setup

1. Read `repo_id` from `refuge.repoid`; derive `repo_name` from the directory name (unchanged).
2. Acquire the exclusive per-repository lock `locks_dir()/<repo_id>.lock` (fs2). This lock
   serializes backups of one repository on this machine. It is held until the manifest is
   published.
3. Create an operation directory `staging_dir()/<repo_id>-<random>/` (tempdir; deleted on drop).
4. `store.sweep_partials()` → warnings.

### 7.2 Read state and catalog

5. `state = git::ref_state(repo)`.
6. `catalog = catalog::load(&layout)` (§8). Let `newest = catalog.newest()` (highest
   `(generation, snapshot_id)` among *parsed* manifests, or `None`).
7. If `newest` exists, `newest.instance_id != config.instance_id` → push warning
   `"newest snapshot <id> was published by another Refuge instance"`.
8. **Idempotency:** if `!options.checkpoint` and `newest` exists and
   `catalog.health(newest) == Valid` and `newest.ref_state_hash == state.hash()` → return
   `AlreadyProtected`.
9. `generation = max(catalog.max_generation_from_manifests(), catalog.max_generation_from_file_names()) + 1`.
10. `snapshot_id = layout::snapshot_id(now, generation, config.instance_id)`.

### 7.3 Choose the parent

`parent = None` (checkpoint) if any of the following holds; otherwise `parent = Some(newest)`:

- `options.checkpoint`
- `newest` is `None`
- `catalog.health(newest) != Valid`
- `newest.refs` has no object refs (empty repository)
- exclusion set (below) is empty or has more than `MAX_EXCLUSIONS` entries
- any exclusion tip is missing locally
- checkpoint policy (§7.4) fires

Exclusion set computation for `newest`: take every object id in `newest.refs` (not `HEAD`);
call `git::batch_check(repo, [oid, "<oid>^{commit}", …])` with two names per tip. For each tip:

- first line `Missing` → tip missing locally → **checkpoint** (this is the split-brain case:
  the newest snapshot was published by another instance from history we do not have; the
  warning from step 7 already explains it).
- second line `Found { oid: peeled, kind: "commit" }` → add `peeled` to the exclusion set.
- second line `Missing` while the first was `Found` → non-commit tip (tag → blob/tree, or a
  blob ref). Skip it; its few objects will simply be re-included in the delta.

Deduplicate the exclusion set.

### 7.4 Checkpoint policy

Given `chain = catalog.chain(newest)` (root first):

- `chain.len() >= 256` → checkpoint.
- `sum(bundle.size for non-root snapshots in chain) >= root.bundle.size` → checkpoint.

Both constants are fixed and documented, not configurable. The byte rule bounds total chain
bytes at 2× the checkpoint; the count rule bounds restore to ≤ 257 bundle files.

### 7.5 Build artifacts in the operation directory

11. Git:
    - `state.refs` empty → `bundle = None`, `kind = Empty`.
    - else if `parent == None` → `git::bundle_create(repo, op/"snapshot.bundle", &[])`;
      `kind = Checkpoint`.
    - else if `!git::has_objects_outside(repo, &exclusions)` → `bundle = None`, `kind = RefsOnly`.
    - else `git::bundle_create(repo, op/"snapshot.bundle", &exclusions)`; `kind = Delta`.
    - When a bundle was created: `git::bundle_verify(repo, bundle)`; `after = git::ref_state(repo)`;
      if `after != state` → refs changed under us: delete the bundle, set `state = after`, and
      repeat from step 8 **once** (second failure → error
      `"repository refs changed while creating the bundle; retry the backup"`). Then require
      `bundle_list_heads(bundle) ⊆ state.refs` (every listed ref exists in `state.refs` with the
      same oid); violation → error (this is an internal consistency check, not a retry case).
    - Compute `Artifact { key: bundle_key(snapshot_id), size, checksum }` via `sha256_file`.
12. LFS:
    - `set = lfs::required_set(repo)`; `after = git::ref_state(repo)`; `after != state` → same
      single-retry rule as above.
    - `set` empty → `lfs = None`.
    - else `(set_bytes, set_artifact) = lfs::encode_set(&set)`;
      `lfs = Some(LfsSection { set: set_artifact, count, size: Σ sizes })`.
    - For every `(oid, size)` in `set`:
      - `store.stat(lfs_object_key(oid))`:
        - `Some(n) if n == size` → reuse; nothing to write.
        - `Some(n)` → error `"target LFS object <oid> has size n, expected size; the backup
          target is damaged — move the file aside and retry"`. Never overwrite.
        - `None` → mark for publication. The local object at `lfs::local_object_path` MUST
          exist and be a regular file, else error `"required LFS object <oid> is missing"`
          (this is the LFS02 scenario: the push succeeded but the object was never uploaded).
13. Build the manifest struct in memory and run `manifest::validate` on it (self-check).

### 7.6 Publish, in this order

14. For each LFS object marked for publication:
    `store.publish_file(local_object_path, lfs_object_key(oid), &Artifact{ key, size, checksum: "sha256:<oid>" })`.
    The source-stream hash check in `publish_file` is what proves the local object matches its
    OID. Accumulate `lfs_bytes_written`, `lfs_objects_written`.
15. Set file: if `store.stat(set.key)` is `None` → `publish_bytes(set_bytes, key)`; if
    `Some(n) != set.size` → damaged-target error as above.
16. Bundle: if `store.stat(bundle.key)` is `Some(_)` → it is an orphan of a crashed attempt
    (no manifest with this id exists, because generation was computed from all filenames) →
    `store.remove(key)`. Then `publish_file(op_bundle, bundle.key, &bundle)`.
17. Manifest: `publish_bytes(serde_json pretty + '\n', "snapshots/<id>.json")`. If this step
    fails, best-effort `store.remove(bundle.key)` (LFS objects and set files stay: they are
    content-addressed and reusable) and return the error.
18. Return `Published`.

Steps 14–16 are idempotent and crash-safe: a crash leaves only content-addressed files or one
orphan bundle, never a discoverable snapshot.

### 7.7 Output (CLI)

```text
protected <snapshot-id> <n> refs (checkpoint, 104857600 git bytes, 629145600 LFS bytes written)
protected <snapshot-id> <n> refs (delta, 204800 git bytes, 0 LFS bytes written)
protected <snapshot-id> <n> refs (refs only, 0 bytes written)
already protected by <snapshot-id>
```

Warnings go to stderr prefixed `refuge: warning:` (unchanged).

## 8. Catalog, health, and protection state (`catalog.rs`)

```rust
pub struct Catalog {
    layout: RepoLayout,
    snapshots: BTreeMap<String /* snapshot_id */, Manifest>,   // parsed & manifest-validated
    diagnostics: Vec<Diagnostic>,                               // unreadable / invalid files
    max_file_name_generation: u64,
    health: RefCell<HashMap<String /* artifact key or snapshot id */, Health>>,   // memo
}
pub enum Health { Valid, Corrupt(String) }
pub struct Diagnostic { pub path: PathBuf, pub kind: DiagnosticKind, pub reason: String }
pub enum DiagnosticKind { Corrupt, Unsupported, Unreadable }

impl Catalog {
    pub fn load(layout: RepoLayout) -> Result<Catalog>;          // reads every snapshots/*.json
    pub fn newest(&self) -> Option<&Manifest>;                    // max (generation, snapshot_id)
    pub fn ordered(&self) -> Vec<&Manifest>;                      // ascending
    pub fn chain(&self, m: &Manifest) -> Result<Vec<&Manifest>, String>;   // root..=m, or reason
    pub fn health(&self, m: &Manifest) -> Health;                 // memoized, §8.3
    pub fn max_generation_from_manifests(&self) -> u64;
    pub fn max_generation_from_file_names(&self) -> u64;
}

pub fn list_targets(target: &Path, filter: Option<&str>) -> Result<Vec<(Uuid, Catalog)>>;
    // enumerate refuge/v2/repos/*, filter by repo id or by repo_name of any manifest
pub enum ProtectionState { Protected { snapshot_id }, Pending, Unprotected, Corrupt { reason } }
pub fn repository_status(config, &HostedRepository) -> Result<ProtectionState>;
pub fn statuses(config, selector: Option<&str>) -> Result<Vec<(HostedRepository, ProtectionState)>>;
```

### 8.1 Loading

- Every entry in `snapshots/` is examined. `*.json` files are parsed and validated (§4.2);
  failures become diagnostics. Any other regular file, and any `.json` whose stem does not
  match its `snapshot_id`, is a `Corrupt` diagnostic (this catches OneDrive conflict copies
  such as `…-DESKTOP-ABC.json`, deliberately loudly).
- `.refuge-*.tmp` files are ignored (they are partials, never snapshots).
- `max_file_name_generation` is computed from every file name via
  `layout::generation_from_file_name`, whether or not the file parsed.

### 8.2 Ordering

`(generation, snapshot_id)` ascending; `newest()` is the last. No timestamps.

### 8.3 Chain resolution and health

`chain(m)`: follow `git.parent`. Each hop MUST: exist in `snapshots`; have the same `repo_id`;
have `generation < child.generation`. Stop at `parent == None`. More than 4096 hops → error.
Any violation → `Err(reason)`.

`health(m)` (memoized by snapshot id; artifact checks memoized by key):

1. `chain(m)` fails → `Corrupt(reason)`.
2. For every snapshot in the chain with a bundle: `store.stat(bundle.key) == Some(bundle.size)`
   else `Corrupt("bundle <key> is missing" | "… has size n, expected m")`.
3. If `m.lfs` is `Some`: `store.stat(set.key) == Some(set.size)`; then `store.read(set.key)` and
   `lfs::decode_set(bytes, &set.checksum)` (content hash is verified here — the file is small);
   `set.len() == count` and `Σ == size`; then for every `(oid, size)`:
   `store.stat(lfs_object_key(oid)) == Some(size)`.
4. Otherwise `Valid`.

Health of one snapshot is O(chain length + LFS object count) `stat` calls. `snapshots list`
computes health for all snapshots; memoization makes the total O(distinct artifacts). `status`
computes health for the newest snapshot only.

### 8.4 Protection state

```text
no manifests and no diagnostics                → Unprotected
any diagnostic                                 → Corrupt { reason: "<path>: <reason>" }
health(newest) == Corrupt(reason)              → Corrupt { reason }
newest.ref_state_hash == live hash             → Protected { snapshot_id }
otherwise                                      → Pending
```

Note the self-healing property: when the newest snapshot is corrupt, the next backup publishes
a checkpoint (§7.3), and the repository returns to `Protected` without user action. Diagnostics
(unreadable or foreign files in `snapshots/`) do not self-heal and require the user to remove
the file; this is intentional.

## 9. Restore (`restore.rs`)

```rust
pub struct RestoreOptions<'a> { selector, snapshot_id: Option<&str>, as_name: Option<&str>, target: &Path, replace: bool }
pub struct RestoredRepository { name, path, snapshot_id, skipped_candidates: Vec<String>, warnings: Vec<String> }
pub fn restore(config: &Config, options: RestoreOptions) -> Result<RestoredRepository>;

/// The verification core, shared with `snapshots verify`: materializes `m` into
/// `staging/repository.git` and returns Ok(()) or Invalid(reason) / Fatal(error).
pub fn materialize(store: &Store, catalog: &Catalog, m: &Manifest, staging: &Path) -> Result<(), CandidateFailure>;
enum CandidateFailure { Invalid(String), Fatal(anyhow::Error) }
```

### 9.1 Candidate selection

Unchanged semantics: resolve the selector to exactly one repository (ambiguous names → error
"restore by repository id"); candidates are all snapshots newest-first, or exactly the one
named by `--snapshot`. A candidate whose `catalog.health` is `Corrupt` is skipped (default
mode, recorded in `skipped_candidates`) or fails the command (explicit mode). Name lock,
identity lock, `--replace`, and the recovery-record logic in the current `restore.rs` are kept
verbatim.

### 9.2 `materialize` (all of this happens in a tempdir under `config.repos_dir`)

1. `chain = catalog.chain(m)` (already valid because health passed; still handle `Err` as
   `Invalid`).
2. For each snapshot `s` in the chain with a bundle: `store.copy_out(s.git.bundle.key,
   staging/"bundles"/"<i>.bundle", &s.git.bundle)`. `Missing`/`Mismatch` → `Invalid`; `Io` →
   `Fatal`. All bundles are copied and verified before any Git command runs.
3. `git::init_bare(staging/"repository.git")`; set `gc.auto=0`, `maintenance.auto=false`
   (via `repo::configure` at the end; but set `gc.auto=0` now too, so nothing can prune
   unreferenced objects between unbundle and ref creation).
4. For each copied bundle in chain order: `git::bundle_unbundle(repo, bundle)`. Failure →
   `Invalid("<snapshot> bundle <key> failed to unbundle: …")`. (`unbundle` runs
   `verify_bundle` first, so a missing prerequisite is detected here.)
5. `git::create_refs(repo, &m.ref_state().refs)` in one transaction. Failure (missing object,
   bad ref) → `Invalid`.
6. If `m.head()` is `Some(target)` → `git::set_symbolic_head`. (`init --initial-branch=main`
   leaves `HEAD → refs/heads/main` otherwise, as today.)
7. LFS: if `m.lfs` is `Some`: read and decode the set (as in health); for each `(oid, size)`:
   `store.copy_out(lfs_object_key(oid), lfs::local_object_path(repo, oid), &Artifact{ key, size,
   checksum: "sha256:<oid>" })`. Create parent directories. `Missing`/`Mismatch` → `Invalid`.
8. Verify:
   - `git::ref_state(repo) == m.ref_state()` and its hash `== m.ref_state_hash`, else
     `Invalid("refs differ from manifest")`.
   - `git::fsck(repo)` (`--full --strict`), else `Invalid`.
   - `lfs::required_set(repo) == decoded set` (or both empty when `m.lfs == None`), else
     `Invalid("LFS set differs from history")`. This independently re-derives the set from
     the restored history, so a wrong set file cannot pass.
9. `repo::configure(repo, m.repo_id)` (identity, `gc.auto`, hook). Return `Ok`.

Disk use during restore: chain bytes (≤ ~2× repository) + the repository + LFS objects. All
under `repos_dir`, never in the target.

### 9.3 Publishing the restored repository

Unchanged: rename staging repository into `<repos_dir>/<name>.git`, or the recovery-record
protocol for `--replace`.

## 10. `snapshots verify` and `snapshots usage` (`verify.rs`)

- `refuge snapshots verify <NAME_OR_REPO_ID> [--snapshot <ID>] [--target <DIR>]`: runs
  `materialize` for the newest (or named) snapshot into a tempdir under `repos_dir`, deletes
  it, prints `verified <snapshot-id>` or `invalid <snapshot-id>: <reason>` and exits 1 on
  invalid. This is the deep check that shallow health does not perform (bundle and LFS object
  content hashes, Git connectivity). Without `--snapshot`, only the newest snapshot is checked.
- `refuge snapshots usage [<NAME_OR_REPO_ID>] [--target <DIR>]`: walks the repository root and
  prints, per repository:

  ```text
  notes 0192d23c-…
    checkpoints   3   314572800 bytes
    deltas      412    52428800 bytes
    lfs objects 5012  629145600 bytes
    lfs sets     37     1638400 bytes
    manifests   415     2097152 bytes
    partials      0           0 bytes   (.refuge-*.tmp)
    total                  … bytes
  ```

  Checkpoints vs deltas are classified from manifests (`git.parent == null`); bundle files
  without a manifest are reported as `orphans`.

## 11. CLI changes (`main.rs`)

| Command | Change |
|---|---|
| `refuge repo backup [<sel>] [--checkpoint]` | new flag; prints per §7.7 |
| `refuge repo status …` | unchanged output; new state derivation |
| `refuge snapshots list [<sel>] [--target]` | columns: `<repo> <snapshot-id> g<gen> <kind> <bundle-bytes> <health>`; `kind ∈ checkpoint, delta, refs-only, empty` |
| `refuge snapshots verify <sel> [--snapshot] [--target]` | new |
| `refuge snapshots usage [<sel>] [--target]` | new |
| `refuge snapshots prune …` | optional milestone, §12 |
| `refuge restore …` | unchanged surface |
| `refuge hook post-receive` | unchanged; handles `AlreadyProtected` by printing `already protected by <id>` |

Server API (`/api/v1/repos`) is unchanged: `protection` and `snapshot_id` come from
`catalog::repository_status`.

## 12. Optional milestone: `snapshots prune`

Only build this after §14 is fully green. Semantics:

```text
refuge snapshots prune <sel> --keep-days <N> [--target <DIR>] [--apply]
```

1. Load the catalog. Let `cutoff` be the lowest generation among snapshots whose
   `created_at ≥ now − N days` (or `newest.generation` if there is none).
   `keep = { newest } ∪ { s | s.generation ≥ cutoff }`. `created_at` is used only to pick the
   cut-off generation; selection is by generation, so clock skew can shift the boundary but
   never reorder snapshots.
2. Close `keep` under `chain()`: every ancestor of a kept snapshot is kept.
3. Delete set = all other snapshots. Reachable artifacts = every bundle key, set key, and LFS
   object of every kept snapshot (decode sets). Unreachable artifacts = everything present under
   `git/`, `lfs/sets/`, `lfs/objects/` that is not reachable.
4. Without `--apply`: print the plan (counts and bytes) and exit 0. With `--apply`: delete
   manifests of the delete set **first** (so no reader can select a snapshot whose data is
   about to disappear), then unreachable artifacts. Never delete anything in `keep` or its
   closure. Print what was deleted.
5. Corrupt snapshots are treated like any other snapshot (kept if new enough, deleted
   otherwise); `prune` is not a repair tool.

## 13. Milestones

Each milestone is independently mergeable with CI green. Suggested branch: `v2-store`.

### M0 — Git semantics spike (tests only, no product code)

Add `tests/git_semantics.rs` using real `git` and `tests/support`. Each test builds fixtures
with plain Git commands and asserts the behaviors this design relies on:

1. `bundle create f --all ^A` where `refs/heads/a == A` (unchanged) and `refs/heads/b` advanced:
   the bundle lists only `refs/heads/b`; `list-heads ⊆ ref state`; unbundling into a repo that
   has `A` succeeds; unbundling into an empty repo fails (prerequisite check).
2. `rev-list --objects --all ^A ^B` is empty after: deleting a branch; rewinding a branch;
   creating a branch on an existing commit. It is non-empty after: a new commit; a new
   annotated tag on an existing commit (the tag object is listed).
3. `bundle create --all ^A` with a new annotated tag on `A` succeeds and the bundle carries the
   tag; restore via `unbundle` + `update-ref --stdin` reproduces `refs/tags/t`.
4. `cat-file --batch-check` with `<oid>` and `<oid>^{commit}` lines for: a commit, an annotated
   tag (peels), a blob (second line `missing`), a missing oid (both lines `missing`).
5. `update-ref --stdin` with `create` lines creates `refs/heads/*`, `refs/tags/*`,
   `refs/notes/*`, and a ref pointing at a blob, in one transaction; a missing object aborts
   the whole transaction.
6. `bundle create` with 512 `^<oid>` arguments succeeds on Windows and Linux.
7. Force push: branch rewritten to an unrelated commit; delta bundle contains only the new
   history; chain replay + `create_refs` reproduces the exact state; `fsck --full --strict`
   passes.

Exit: all seven pass on both CI platforms. Any failure changes §6.1/§7 before M1 begins.

### M1 — Foundations

`layout.rs`, `store.rs`, `manifest.rs` (v2), `git.rs` additions, `lfs.rs` rewrite. Unit tests:

- layout: every key shape; `resolve` rejects `..`, absolute paths, backslashes, wrong shapes;
  `generation_from_file_name`.
- store: publish verifies source hash, re-reads, never overwrites; `.tmp` naming; `copy_out`
  classifies Missing / Mismatch / Io; `sweep_partials` deletes only old `.refuge-*.tmp`.
- manifest: every rule in §4.2 has a negative test; round-trip serialization.
- lfs: `encode_set`/`decode_set` strictness; `required_set` on a fixture with a pointer, a
  non-pointer small blob, a large blob, and the same pointer in two commits.
- git: each new function against a fixture (can reuse M0 fixtures).

Because `manifest.rs` changes shape, the old `backup.rs`/`restore.rs`/`discovery.rs` stop
compiling as soon as M1 lands. M1 and M2 therefore ship as one PR on the `v2-store` branch,
with one commit per module and the whole suite green at the end of M2.

### M2 — Backup, catalog, restore on v2

Replace `backup.rs`, `discovery.rs → catalog.rs`, `restore.rs`; delete `storage.rs`, tar
code, `tar` dependency; update `main.rs`, `server.rs` printing, `repo.rs` lock dir. Update
existing integration tests and BDD steps for the new layout and outputs.

New integration tests (`tests/v2_store.rs` or split by topic), each through the CLI:

1. **Lifecycle:** create → checkpoint; push → delta; push again → delta; `status` Protected
   after each; `snapshots list` shows kinds; total bytes in `git/` after three pushes ≈
   checkpoint + two small deltas (assert deltas < 10% of checkpoint for a fixture that adds one
   small file per push).
2. **Refs-only snapshots:** delete a branch → new manifest with `bundle: null`; create a branch
   on an existing commit → same; restore each → exact refs.
3. **Force push and rewind:** restore reproduces exact refs; `fsck` passes.
4. **Annotated tag, notes ref, tag → blob:** restorable.
5. **Empty repository:** `Empty` manifest; then first push → checkpoint.
6. **Idempotency:** `repo backup` when protected → `already protected`, no new manifest;
   `--checkpoint` → new checkpoint with identical refs.
7. **Checkpoint policy:** fixture with a tiny checkpoint and pushes that exceed its size →
   next snapshot is a checkpoint; 256-count rule via a unit test on the policy function with a
   synthetic catalog (do not push 256 times in an integration test).
8. **LFS:** push with an LFS object → object under `lfs/objects/`, set under `lfs/sets/`;
   Git-only push → no new bytes under `lfs/`; new LFS object → exactly one new object and one
   new set; restore → objects match; missing local object → backup fails, `Pending`; after
   upload, retry → `Protected` (existing LFS02 scenario).
9. **Corruption — middle delta truncated:** snapshots from that delta onward are `corrupt`
   in `list`; `status` is `Corrupt`; default restore falls back to the last snapshot before
   the damaged delta and reports skipped ids; explicit restore of a corrupt snapshot fails
   without creating a repository; next push → checkpoint → `Protected`.
10. **Corruption — LFS object deleted from target:** newest `corrupt`; restore falls back to
    a snapshot whose set does not include it (or fails if all do); next backup republishes the
    object and returns to `Protected`.
11. **Corruption — wrong-size LFS object in target:** backup fails with the damaged-target
    message; nothing overwritten.
12. **Damaged manifest / conflict copy in `snapshots/`:** `status` is `Corrupt` naming the
    file; `list` shows the diagnostic.
13. **Crash between bundle and manifest** (simulate by publishing a bundle file with the
    would-be id and no manifest): next backup removes the orphan and succeeds.
14. **Concurrent ref change during bundle creation:** reuse the existing barrier-style unit
    test; assert one retry and a consistent manifest.
15. **Split brain:** two instances (two configs, two instance ids) sharing one target; B
    restores A's repo, both push; each publishes a checkpoint with a foreign-writer warning;
    both lineages restore correctly by snapshot id.
16. **No temporary files in the target:** after every test scenario, assert no `.tmp` files
    and no `.refuge-*` directories exist under `<target>`, and no lock files exist there.
17. **Clean-machine restore (server mode):** existing `server_cli` restore-on-empty-runtime
    test passes on v2.

Exit: all tests green on both platforms; `cargo build` has no reference to `storage`,
`discovery`, `tar`, `create_archive`, `lfs_artifact`, `repo.json`.

### M3 — `verify`, `usage`, output polish

`verify.rs`, CLI wiring, README and `docs/engineering/manifest-v2.md` (replace v1 doc), update
`2026-09-19-incremental-backup-redesign.md` status to "Superseded by this plan". Tests:
`verify` passes on a valid snapshot and fails with a reason on a byte-flipped bundle whose size
is unchanged (this is the case shallow health cannot see); `usage` numbers equal the sum of
file sizes on disk.

### M4 — Benchmark and real-repository validation

Extend `scripts/benchmark.sh` to a fixture with LFS objects; record checkpoint size, delta
size, LFS bytes written per push, backup wall time, restore wall time, and file counts in
`docs/engineering/2026-09-19-performance-baseline-v2.md`. Run one manual end-to-end cycle on the
real Obsidian repository (no fixture data recorded): first backup, ten pushes, clean-machine
restore, `verify`. This milestone has no code deliverable except the script.

### M5 — Optional: `snapshots prune` (§12)

Tests: plan output without `--apply` changes nothing; with `--apply`, kept snapshots and their
chains remain restorable; every deleted artifact was unreachable; the newest snapshot is never
deleted even with `--keep-days 0`.

## 14. Acceptance checklist (Definition of Done for the rewrite)

1. An unchanged LFS object set is not republished after a Git-only push (zero bytes under
   `lfs/` change).
2. After the first checkpoint, an ordinary push publishes Git payload proportional to its new
   objects.
3. Every manifest either restores completely or is deterministically classified `corrupt`
   with a reason naming the missing or invalid dependency.
4. Restore reproduces the selected manifest's exact refs and symbolic `HEAD`, including after
   force pushes, rewinds, ref deletion, new refs on existing commits, annotated tags, notes,
   and refs to non-commit objects.
5. A clean-machine restore verifies with `git fsck --full --strict` and verifies every LFS
   object by size and SHA-256, and independently re-derives the LFS set from history.
6. `repo backup` is idempotent when already protected.
7. Nothing other than final, immutable files ever appears in the target (no locks, no staging
   directories, no partials without `.tmp`).
8. `snapshots usage` explains total target consumption; `repo backup` output states bytes
   newly written.
9. `snapshots verify` detects content corruption that shallow health does not.
10. Documentation states that local publication into the sync folder is not confirmation of
    cloud upload (unchanged claim).
11. No code path reads or writes the v1 layout, `lfs.tar`, or `repo.json`.

## 15. Non-goals of this rewrite

- Encryption, Microsoft Graph adapter, upload confirmation probes.
- Push coalescing or quiet periods (deltas are cheap; idempotent backup suffices).
- Disk-space preflight checks.
- Sharing LFS objects across repositories.
- Any local index, cache, or database of the catalog. If `status` on thousands of manifests
  ever becomes slow, `prune` is the remedy, not a cache.
- Automatic repair of damaged content-addressed files. Backup fails loudly; the user moves
  the file aside.

## 16. Sources relied upon

- Git, **git-bundle**: incremental bundles via revision exclusion, prerequisites, `verify`,
  `unbundle`, `list-heads`. <https://git-scm.com/docs/git-bundle>
- Git, **git-rev-list**: `--objects`, exclusion, object listing semantics.
  <https://git-scm.com/docs/git-rev-list>
- Git, **git-cat-file**: `--batch-check`, `--batch`, `<rev>^{commit}` peeling.
  <https://git-scm.com/docs/git-cat-file>
- Git, **git-update-ref**: `--stdin` transactions. <https://git-scm.com/docs/git-update-ref>
- Git LFS specification: pointer format and content-addressed object layout.
  <https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md>
- Microsoft, **Block syncing of specific file types** (`.tmp` is not uploaded by the sync
  client). <https://learn.microsoft.com/en-us/sharepoint/block-file-types>
- Microsoft, **Restrictions and limitations in OneDrive and SharePoint** (item counts,
  conflict behavior).
  <https://support.microsoft.com/en-us/onedrive/restrictions-and-limitations-in-onedrive-and-sharepoint>
