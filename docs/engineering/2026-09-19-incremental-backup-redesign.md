---
title: Refuge Incremental Backup Redesign
status: Superseded by 2026-09-19-v2-snapshot-store-implementation-plan.md
date: 2026-09-19
outline: |
  - L1 Re-evaluate the assumption that live bare repositories must not be synchronized through OneDrive
    - L2 Git's local atomicity does not extend to a file-by-file cloud synchronization service
      - L3 Git and Microsoft documentation both describe partial-update and conflict risks
    - L2 The risk does not require every recovery point to contain a complete copy of all data
      - L3 Git bundles can be incremental and Git LFS objects are already immutable and content-addressed
  - L1 Replace full per-push artifacts with immutable shared data and small snapshot manifests
    - L2 Publish Git LFS objects once by SHA-256 object ID
    - L2 Publish incremental Git bundles between periodic full checkpoints
    - L2 Keep manifest-last publication as the recovery-point transaction boundary
  - L1 Deliver the redesign incrementally without invalidating existing backups
    - L2 Add a backward-compatible LFS object-set representation first
    - L2 Add incremental bundle chains and checkpoint compaction second
    - L2 Add explicit retention, garbage collection, and OneDrive fault testing last
---

# Refuge Incremental Backup Redesign

## 1. Decision summary

Refuge should continue to keep its live, writable bare repositories outside OneDrive. It should
not, however, continue to create a complete Git bundle and a complete Git LFS archive after every
push.

The original design joined two separate claims:

1. An active Git repository is not a safe unit for a general-purpose file synchronization service.
2. Every remotely recoverable state must therefore be a complete, self-contained snapshot.

The first claim remains well supported. The second does not follow from it and is the source of
the current scaling problem.

The proposed replacement keeps the separation between the live repository and the backup target,
but changes the backup target into an append-only collection of immutable shared data:

- Git LFS objects stored once under their SHA-256 object IDs.
- Incremental Git bundle segments between periodic full bundle checkpoints.
- Small snapshot manifests that record the complete ref state, required LFS object set, parent or
  checkpoint relationship, and artifact checksums.
- Manifest-last publication, which remains the transaction boundary that makes a recovery point
  discoverable.

In this model, storage and upload volume grow primarily with new repository content, rather than
with repository size multiplied by push count.

## 2. Background and observed failure

The design problem was exposed by a real Obsidian repository with approximately:

- 100 MB of ordinary Git-managed data; and
- 600 MB of Git LFS data.

The repository is pushed to Refuge several times on an active day. The current implementation
creates roughly 700 MB of new backup artifacts for every push. At three to five pushes per day,
that is approximately 63 to 105 GB of new OneDrive content per 30-day month. A nominal 1 TB
OneDrive allocation can therefore be consumed by this repository alone in roughly 10 to 16
months, before accounting for unrelated files or provider-side version storage.

This is not an isolated performance bug. It follows directly from the current artifact model:

- `git::bundle_create` always invokes `git bundle create <destination> --all`, so every non-empty
  backup contains all objects reachable from all refs.
- `lfs::create_archive` enumerates every object in the hosted repository's `lfs/objects` store and
  writes all of them to a new tar archive.
- Every backup receives a new snapshot ID and publishes newly named bundle and LFS artifacts.
- Retention defaults to keeping every snapshot.

Because each artifact has a unique name, OneDrive sees a new 700 MB logical file set rather than
a modification of an existing file. Chunked transfer may change how the provider transports a
large file, but it cannot make separately named full copies consume only incremental logical
storage.

The implementation plan had already listed full-bundle size as a risk and incremental bundles as
a future mitigation. The real repository shows that incremental storage is not a later
optimization: it is part of the minimum viable data model for repositories with LFS content.

## 3. Re-evaluating direct synchronization of a bare repository

### 3.1 Why a bare repository appears attractive

A bare repository is already an incremental data structure. A normal push usually adds new
objects or a new pack and changes a small number of refs. Git LFS similarly stores new content
under content-derived object IDs. In the common case, this creates far less filesystem churn than
publishing a new full bundle and tar archive.

A bare repository also removes working-tree concerns such as editor caches, generated files, the
index, and machine-specific untracked content. These properties make direct OneDrive
synchronization substantially more plausible than synchronizing a working tree and its `.git`
directory together.

They do not make it transactionally safe.

### 3.2 Git's on-disk transaction boundary is local

Git documents a bare repository as the repository form normally used for exchanging history by
push and fetch. Its layout nevertheless consists of many independently visible files: loose
objects, packfiles and indexes, refs, `packed-refs`, reflogs, configuration, and administrative
state [1].

Git protects local ref updates with lock files and atomic replacement. A prepared multi-ref
transaction locks all queued refs before committing them, but Git explicitly notes that a
concurrent reader may still observe only a subset of the modifications [2]. This guarantee is
defined in terms of processes using one filesystem. It does not cause a cloud client to upload all
files belonging to the transaction as one unit.

Repository maintenance creates another boundary. Repacking can create new packfiles, update
indexes, and later delete obsolete loose objects or packs. Git's maintenance documentation
describes careful multi-step ordering to reduce races [3]. The `git gc` documentation still warns
that concurrent writers and pruning retain a residual risk of repository corruption [4]. A file
synchronizer can independently observe and propagate the additions, metadata replacements, and
deletions from those steps.

Most importantly, the Git FAQ directly advises against using a cloud synchronization service for
any portion of a Git repository. It explains that file-by-file continuous synchronization does
not understand repository structure and can produce missing objects, broken refs, partial
updates, and data loss. For directory-level copying, Git recommends a quiescent repository,
exact-copy semantics, subsequent verification, and an independent backup [5]. The `git bundle`
manual gives the same reason for discouraging a naive recursive copy while the source repository
may be changing [6].

### 3.3 OneDrive provides file synchronization, not repository transactions

Microsoft documents different synchronization paths by file type. For general files, items of at
least 8 MB are divided into chunks and transferred through a BITS session, while other changes
are batched into HTTPS requests [7]. This is a transport description, not a guarantee that a
multi-file Git update appears atomically in OneDrive.

Microsoft also documents that simultaneous changes from multiple computers, web uploads during
synchronization, and offline edits can cause synchronization conflicts [8]. That is incompatible
with using the synchronized directory as a multi-writer Git remote.

OneDrive and SharePoint impose additional operational considerations:

- Microsoft warns that synchronizing more than approximately 300,000 items can take a long time
  [8]. A design that exports every Git object as a separate loose file would therefore trade byte
  efficiency for a potentially serious item-count problem.
- OneDrive and SharePoint retain file version history according to account and administrator
  policy. Microsoft describes version history as a source of storage quota consumption and
  provides automatic or manual trimming controls [9]. Repeatedly overwriting one large bundle is
  therefore not a reliable solution to storage growth either.
- The sync client does not upload some file types, including `.tmp` and `.ini`, and administrators
  can configure additional exclusions [10]. A backup format should not depend on arbitrary Git
  implementation temporary files being transferred.

### 3.4 Where a synchronized bare repository is usable

The risk is conditional rather than magical. A synchronized bare repository often converges to a
valid copy when all of the following are true:

- exactly one machine ever writes it;
- no other machine opens or modifies its synchronized copy;
- automatic maintenance and destructive pruning are disabled or tightly controlled;
- the writer becomes quiescent long enough for all changes to upload; and
- recovery verifies the downloaded result with `git fsck` before use.

This can be a practical best-effort secondary copy. It is not a strong disaster-recovery
contract, because Refuge cannot identify which cloud-visible moment contains a coherent set of
objects and refs. If the source machine is lost while OneDrive has propagated only part of an
update, the cloud directory may contain no explicitly marked last-known-good state.

Multi-writer use is categorically worse: two machines can independently advance the same ref,
and file conflict handling cannot perform Git's reachability negotiation or preserve the intended
winner.

The revised conclusion is therefore:

> A live Git repository should not rely on a general-purpose file synchronizer for repository
> consistency. Immutable Git and LFS data are nevertheless excellent synchronization units when
> Refuge publishes them under an explicit manifest transaction.

## 4. Why the current snapshot model is stronger than necessary

The useful safety properties of the current model are:

- the live repository is outside the synchronized folder;
- artifact paths are immutable and never overwritten;
- artifacts are checksummed and re-read before publication;
- a manifest is written last and is the only marker that makes a snapshot discoverable;
- restore validates content and ref state before replacing a hosted repository; and
- a corrupt newest snapshot can be rejected in favor of an older valid snapshot.

None of these properties requires every manifest to reference a newly duplicated copy of all
reachable data.

The over-strong property is *snapshot self-containment*: one bundle and one LFS tar must be able to
restore the repository without consulting any earlier snapshot or shared object. Self-containment
makes one snapshot easy to reason about, but converts push frequency directly into storage and
bandwidth consumption.

Git's own bundle format supports both full and incremental bundles. An incremental bundle omits
objects known to be present at the destination and records prerequisite commits. Git provides
`git bundle verify` to check those prerequisites [6]. The product can therefore preserve standard
Git artifacts without producing a full bundle on every push.

Git LFS is an even clearer case. The official Git LFS specification stores actual content under
`.git/lfs/objects/<oid[0:2]>/<oid[2:4]>/<oid>`, where the OID is the SHA-256 digest of the content.
The object is created through a temporary file and atomically moved into its content-addressed
location [11]. Repacking all such immutable objects into a fresh tar after every push discards the
deduplication property that LFS already provides.

## 5. Proposed storage model

### 5.1 Logical layout

The precise field names remain subject to an implementation spike, but the intended layout is:

```text
<target>/refuge/v1/repos/<repo-id>/
  repo.json
  git/
    checkpoints/<artifact-id>.bundle
    deltas/<artifact-id>.bundle
  lfs/
    objects/sha256/<oid[0:2]>/<oid[2:4]>/<oid>
    sets/<set-hash>.json
  snapshots/<snapshot-id>.manifest.json
```

Every data file is immutable after publication. Different snapshot manifests may reference the
same checkpoint, delta, LFS set, or LFS object.

### 5.2 Snapshot manifest responsibilities

A new-format manifest should describe:

- the complete ref state and symbolic `HEAD` for that recovery point;
- the ref-state hash;
- whether the Git artifact is a full checkpoint or an incremental segment;
- the parent snapshot and/or checkpoint from which restoration begins;
- artifact size and SHA-256 checksum;
- the required LFS object set, directly or through a content-addressed set index;
- the Refuge instance that published it; and
- the existing schema, encryption, and version metadata.

The manifest remains the publication marker and is always committed last. An uploaded object or
bundle without a manifest is merely an unreferenced artifact. A manifest with a missing or
invalid dependency is corrupt and is never selected for restore.

Manifest v1 already says that writers may add fields and readers must ignore unknown fields.
Existing `artifact` and `lfs_artifact` snapshots therefore remain readable while new optional
fields introduce the shared-object representation. A schema-version change is needed only if an
existing field's meaning must change rather than being supplemented.

### 5.3 Git LFS object publication

For each candidate snapshot, Refuge should:

1. Enumerate LFS pointers reachable from the captured ref state.
2. Validate every required local object against its declared size and SHA-256 OID.
3. Compute the immutable target path from the OID.
4. Publish only target objects that are absent; verify an existing object's size and checksum
   before reusing it.
5. Publish a deterministic object-set index containing the required `(oid, size)` pairs.
6. Reference that set from the final snapshot manifest.

Restore downloads or reuses the listed objects, places them into the normal Git LFS object
layout, and repeats size and SHA-256 verification.

For the motivating repository, the initial backup still writes approximately 600 MB of LFS
data. A subsequent push with no new LFS content writes no additional LFS payload. A changed LFS
file adds only its new complete object, which matches normal Git LFS semantics.

Garbage collection must be explicit and conservative. An LFS object may be removed only when no
retained snapshot object set references it. The default should remain to retain data until the
user selects and confirms a retention operation.

### 5.4 Incremental Git bundle publication

The first protected generation creates a full bundle checkpoint. Later generations create a
bundle containing objects newly required relative to the last protected recovery state. The
manifest still records the complete desired refs; ref deletion or rewind is state in the
manifest, not an instruction to delete data from an older bundle.

Git officially supports incremental bundles through revision exclusions and prerequisites [6],
but the implementation must be validated against Refuge's broader ref model. A prototype must
cover branches, lightweight and annotated tags, notes, symbolic `HEAD`, non-commit refs, force
pushes, ref deletion, and newly created refs that point to objects already present in the chain.

Restoration should:

1. Find the selected manifest's nearest full checkpoint.
2. Verify the checkpoint checksum and bundle.
3. Follow the manifest dependency chain in order.
4. Verify every incremental bundle and its prerequisites before applying it.
5. Materialize the complete object database in an isolated bare repository.
6. Set refs exactly to the selected manifest state, including deleting refs not present there.
7. Restore the symbolic `HEAD`.
8. Restore and verify the selected LFS object set.
9. Compare the resulting ref-state hash with the manifest.
10. Run `git fsck --full --strict` before publishing the restored repository.

Refuge should create a new full checkpoint when any configured threshold is reached. Reasonable
initial thresholds for measurement are:

- 30 days since the last checkpoint;
- 100 incremental segments;
- cumulative incremental bytes exceeding 50 percent of an estimated full bundle; or
- an explicit `checkpoint` or `compact` command.

These are starting values, not compatibility requirements. Restore cost and real repository
measurements should determine the final defaults.

### 5.5 Why not synchronize loose Git objects directly

Publishing every Git object by object ID would maximize byte-level deduplication, but it would
create large numbers of small OneDrive items, require Refuge to reconstruct loose-object encoding
for objects currently stored in packs, and reimplement more of Git's object transfer behavior.

Incremental bundles retain standard Git pack semantics, bound the number of cloud files, and can
be created and verified by Git itself. They are therefore the preferred first implementation.

## 6. Retention semantics

The redesign requires an explicit product answer to a previously hidden question: is Refuge a
disaster-recovery system or a permanent archive of the exact ref state after every push?

The recommended default is disaster recovery with a recovery window:

- preserve all history reachable from the current refs;
- preserve recent prior ref states for a configurable period, initially 30 days;
- retain small manifests longer than their unshared payload where practical;
- never remove an object referenced by a retained manifest;
- never automatically delete the newest verified recovery chain; and
- require an explicit command and preview for remote garbage collection.

This preserves recovery from accidental force pushes and branch deletion without permanently
turning every push into another full repository copy. Users who require a permanent push-by-push
archive can select a keep-all policy; shared objects still prevent duplicate content from being
stored repeatedly.

Retention and compaction are separate:

- Checkpoint compaction reduces restore-chain length but must not silently remove old recovery
  points.
- Retention decides which recovery points remain supported.
- Garbage collection removes artifacts that are unreachable from all retained recovery points.

## 7. Delivery plan

### Phase 0: measurement and format spike

- Capture a representative sequence of 10 to 100 pushes from the Obsidian repository without
  recording sensitive content in fixtures or logs.
- Record full-bundle size, incremental-bundle size, LFS object additions, creation time, restore
  time, artifact count, and target bytes written.
- Prototype incremental bundles for all supported ref types and history rewrites.
- Finalize additive manifest fields and dependency validation rules.

Exit criterion: a written fixture demonstrates full restore after normal pushes, force pushes,
ref deletion, and a corrupt or missing middle segment.

### Phase 1: content-addressed LFS objects

- Add the shared LFS object and object-set layout.
- Keep reading and restoring existing `lfs_artifact` tar snapshots.
- Publish only missing LFS OIDs for new snapshots.
- Verify reused target objects instead of trusting path existence alone.
- Add backup, discovery, restore, corruption, and backward-compatibility tests.
- Report total unique LFS bytes and newly published LFS bytes separately.

This phase provides the largest immediate benefit: it removes approximately 600 MB of repeated
data from a typical unchanged-LFS push in the motivating repository.

### Phase 2: incremental Git bundles

- Add checkpoint and delta artifact descriptors.
- Derive each delta from the last fully published recovery state, never from an uncommitted local
  attempt.
- Preserve complete ref state in every manifest.
- Restore through a validated dependency chain.
- Fall back to a new full checkpoint when a safe incremental bundle cannot be constructed.
- Test pushes arriving while bundle creation is in progress and foreign-writer branches in the
  manifest graph.

Exit criterion: after one initial full backup, ordinary small pushes publish payload proportional
to newly introduced Git objects and restore successfully on a clean machine.

### Phase 3: retention, compaction, and observability

- Add target usage reporting split into checkpoints, deltas, unique LFS objects, manifests, and
  reclaimable data.
- Add a dry-run retention planner and an explicit garbage-collection command.
- Add checkpoint thresholds and bounded restore-chain policy.
- Coalesce rapid successive pushes with a short quiet period while keeping protection status
  honest.
- Never report a generation as protected until its complete dependency graph is locally present
  and verified.

### Phase 4: OneDrive fault testing

- Test with a real OneDrive Business sync client paused and resumed at every publication stage.
- Test source-machine loss after data publication but before manifest publication and after
  manifest publication but before all cloud upload completes.
- Test Files On-Demand rehydration during restore.
- Test name conflicts, offline edits, missing segments, truncated artifacts, and delayed deletes.
- Measure actual network transfer independently from logical target bytes where tooling permits.

The filesystem target can only prove local publication. Strict cloud confirmation remains a
future provider-adapter capability; status and documentation must continue to distinguish those
two claims.

## 8. Non-goals and rejected shortcuts

### Put the live bare repository back into OneDrive

Rejected as the primary backup contract. It recovers logical incrementality but loses an explicit
cloud-visible transaction boundary and contradicts Git's documented guidance. It may remain a
documented best-effort mode only if Refuge clearly states the single-writer, quiescence, and
verification limitations.

### Overwrite one full bundle repeatedly

Rejected. It still creates full bundles and full local writes, relies on provider-specific delta
transfer behavior, and may accumulate provider file versions that consume quota. It also offers
weaker independent recovery points than immutable artifacts.

### Keep only the latest full snapshot

Rejected as the main solution. It limits steady-state logical storage only by continuously
uploading and deleting full repository copies, retains the same bandwidth problem, and exposes
recovery to synchronization and deletion delays.

### Compress the LFS tar

Rejected as an architectural fix. Many LFS formats are already compressed, encrypted, or binary,
and unchanged objects would still be repackaged and uploaded after every push.

### Depend on OneDrive version history as Refuge retention

Rejected. Provider versioning is configured outside Refuge, operates per file rather than per
repository recovery point, and does not establish coherent Git object/ref generations.

## 9. Acceptance criteria

The redesign is complete when all of the following are true:

1. An unchanged 600 MB LFS object set is not republished after a Git-only push.
2. After the first checkpoint, a small ordinary push publishes Git payload proportional to its
   new objects rather than to the full 100 MB repository.
3. Every visible snapshot manifest either restores completely or is deterministically classified
   as corrupt because a dependency is absent or invalid.
4. Restore reproduces the selected manifest's exact refs and symbolic `HEAD`, including after
   force pushes and ref deletion.
5. Existing full-bundle and LFS-tar snapshots remain discoverable and restorable.
6. No garbage-collection operation deletes an artifact reachable from a retained manifest.
7. A clean-machine restore verifies Git with `git fsck --full --strict` and verifies every required
   LFS object by size and SHA-256.
8. Usage reporting explains both total target consumption and bytes newly written by the most
   recent backup.
9. Documentation does not equate local publication into the sync folder with confirmed OneDrive
   upload.

## 10. Sources

Sources were reviewed on 2026-09-19.

1. Git, **gitrepository-layout** — repository forms and the on-disk object/ref layout:
   <https://git-scm.com/docs/gitrepository-layout>
2. Git, **git-update-ref** — ref locking, transactions, and partial visibility to concurrent
   readers: <https://git-scm.com/docs/git-update-ref>
3. Git, **git-maintenance** — multi-step loose-object and incremental-repack maintenance:
   <https://git-scm.com/docs/git-maintenance>
4. Git, **git-gc** — pruning and residual corruption risk with concurrent writers:
   <https://git-scm.com/docs/git-gc>
5. Git, **gitfaq**, “Transfers” — explicit warning against cloud synchronization of Git
   repositories and quiescent-copy guidance: <https://git-scm.com/docs/gitfaq>
6. Git, **git-bundle** — full and incremental bundles, prerequisites, verification, and the risk
   of copying a repository while it is being written: <https://git-scm.com/docs/git-bundle>
7. Microsoft, **How sync works** — general-file batching and chunked upload behavior:
   <https://learn.microsoft.com/en-us/sharepoint/sync-process>
8. Microsoft, **Restrictions and limitations in OneDrive and SharePoint** — item-count guidance
   and synchronization conflict scenarios:
   <https://support.microsoft.com/en-us/onedrive/restrictions-and-limitations-in-onedrive-and-sharepoint>
9. Microsoft, **Plan version storage on document libraries** — OneDrive/SharePoint version
   retention and quota impact: <https://learn.microsoft.com/en-us/sharepoint/plan-version-storage>
10. Microsoft, **Block syncing of specific file types** — files omitted by the sync client and
    administrator-configured exclusions: <https://learn.microsoft.com/en-us/sharepoint/block-file-types>
11. Git LFS, **Specification** — pointer format and content-addressed local object layout:
    <https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md>

## 11. Relationship to existing documents

This proposal revises the storage-efficiency assumptions in the product requirements, technical
review, implementation plan, and iterative delivery path. It does not change the currently frozen
Manifest v1 reader contract or invalidate artifacts already published by released Refuge
versions.

If the proposal is accepted, the follow-up documentation change should:

- amend the PRD requirement for a “full repository snapshot” to permit a verified checkpoint and
  dependency chain;
- promote incremental backup from a future optimization to a core invariant;
- define snapshot retention independently from artifact garbage collection; and
- update the active delivery path so content-addressed LFS storage precedes further feature work.
