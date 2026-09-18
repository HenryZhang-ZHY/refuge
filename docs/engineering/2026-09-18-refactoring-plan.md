---
title: Refuge Technical Debt Assessment and Refactoring Plan
status: Proposed
date: 2026-09-18
baseline: c11ba68
reference: tgrep b1d0fc2
---

# Refuge Technical Debt Assessment and Refactoring Plan

## 1. Recommendation and scope

Refactoring is worthwhile. Prioritize snapshot publication, verification, and restore transactions, then consolidate application workflows, Git execution, and test infrastructure. The project has 1,910 lines under `src`, already separates its library and binary, and organizes modules by feature. Keep one crate and extract responsibilities incrementally instead of immediately implementing the older seven-crate architecture.

The urgent issue is the lack of centralized contracts for snapshot completeness, successful publication, safe retries, and the generation used during restore. A daemon, automatic retries, encryption, and remote storage would amplify these ambiguities.

This document provides analysis and an execution plan only. Tasks include necessary bug fixes and are not all behavior-preserving refactors. Changes to default restore fallback, error messages, or durability states should have separate commits and documentation updates.

Evidence comes from current source and tests, existing product and engineering documents, the sibling tgrep checkout, and official references. Findings labeled "confirmed by code inspection" follow from control flow or path construction; they do not imply that concurrency, power-loss, or Windows reproductions have already been run. Implementation agents should first add failing regression cases.

## 2. Baseline and existing strengths

Preserve manifest-last publication, per-repository OS backup locks, streaming SHA-256, bundle ref comparisons, temporary restore directories with same-volume rename, real Git integration tests, first-use BDD, and the product boundary that confirms local backup without claiming cloud upload confirmation.

Checks performed on Linux during this assessment:

| Check | Result |
| --- | --- |
| `cargo test --locked` | Three backup_cli tests and five first-use BDD scenarios passed; bdd_lfs then failed because git-lfs was unavailable |
| Separate runs of cli, cli_guidance, repo_cli, git_roundtrip, lfs_backup_restore, and restore_cli | All 32 tests passed; including backup_cli, 35 ordinary integration tests passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo fmt --all -- --check` | Failed on existing formatting differences in config.rs and restore.rs; neither was changed during the assessment |
| Windows, macOS, and actual cloud sync clients | Not verified |

Cargo was available at `/home/nm/.cargo/bin/cargo` but absent from the default PATH, so checks used its absolute path. Git was 2.39.5, below the older implementation plan's stated minimum of 2.40. Passing these tests does not establish full minimum-version or platform compatibility. Both source unit-test targets contained zero tests; coverage is concentrated at the CLI integration layer.

## 3. Technical debt inventory

### D1 · P0: Staging files can collide across repositories

Confirmed by code inspection: `src/backup.rs:30` locks by repo_id, but staging uses a shared target directory. The snapshot_id contains a timestamp with second precision, generation, and instance prefix, without repo_id. Two repositories backing up the same generation within the same second share their `<snapshot_id>.bundle` and `.lfs.tar` staging paths. The create_artifact and create_lfs_artifact functions also remove existing staging files with those names.

Independent backups can consequently remove, overwrite, or read each other's intermediate files. A future limit of one worker per repository would not address this collision.

Approach: give every operation an exclusively created temporary directory associated with repo_id, containing all intermediate bundle and LFS files. Preserve the manifest v1 destination layout; fixing staging does not require changing the public snapshot_id format. Separately ensure publication cannot overwrite an existing snapshot.

### D2 · P0: Publication lacks explicit durability and failure stages

Confirmed by code inspection: `backup.rs:245`, `:312`, and `:337` respectively publish artifacts, publish JSON, and ignore every sync_all error. Checksums are calculated before copying, with no destination reread verification or explicit directory synchronization policy. Failure to delete the source after artifact publication returns an error even though the destination has already changed.

Distinguish atomic visibility, local storage durability, and completed cloud synchronization. Omitting cloud verification does not justify unconditionally ignoring local flush errors. Rename alone does not prove consistency across power loss.

Approach: centralize FsSnapshotStore and publication operations. Write a unique partial file, verify the bytes actually written, synchronize according to platform policy, publish artifacts, and publish the manifest last. Represent NotCommitted, Committed, CommittedWithWarnings, or uncertain durability explicitly. Cleanup failure must not be reported as absence of publication. Ordinary I/O errors must not be silently classified as unsupported flushing in a sync folder.

### D3 · P0: Manifest deserialization substitutes for validation

Confirmed by code inspection: `manifest.rs:89` only deserializes JSON. There is no centralized validation of schema_version, artifact format/version, encryption, snapshot filename versus envelope, or refs versus ref_state_hash. `discovery.rs:82` parses every manifest before filtering repositories, so one damaged JSON file can interrupt listing or restoration for unrelated repositories. `backup.rs:285` can likewise be blocked by damaged historical JSON.

SnapshotHealth::Valid currently establishes artifact presence and matching size, not checksum validity, Git object integrity, or LFS completeness. Artifact keys already receive lexical path validation, which should be preserved; symbolic-link escape and external metadata validation remain separate concerns.

Approach: distinguish raw manifests, manifests with validated structure and semantics, and snapshots with verified content. Distinguish corruption, unsupported versions or encryption, and unreadable entries. Return per-entry diagnostics with paths, while retaining fatal errors for failures such as an unreadable target root. Fault tolerance must not silently discard all errors or treat unknown versions as corrupt files eligible for deletion.

### D4 · P0: Restore selection and complete verification cannot provide full fallback

Confirmed by code inspection: `restore.rs:103` selects a snapshot using presence and size, while `:25` checks its checksum later. If the selected newest artifact is damaged without changing size, restoration fails without trying an older snapshot. The README promises the newest valid snapshot; fallback currently covers only missing artifacts or size mismatches detected earlier.

The assertion around `tests/restore_cli.rs:134` actually requires default restoration to fail on same-size tampering. Split it into two contracts: an explicitly selected corrupt snapshot fails, and default selection may fall back with an explanation.

Restoration also lacks a final refs/hash equality check and explicit git fsck. Git bundle verify must not be treated as complete validation of restored content.

Approach: verify and stage each candidate before publication. Content corruption may permit trying an older candidate; environmental failures such as permissions or insufficient disk space should stop the operation. Report the chosen snapshot_id, skipped candidates, and the possibility of missing newer history. Explicit --snapshot selection must never silently substitute another snapshot.

### D5 · P0: LFS validation checks existing objects but does not establish completeness

Confirmed by code inspection: `backup.rs:146` traverses the bare repository's existing lfs/objects without deriving required pointers from history reachable through the backed-up refs. An empty directory produces a snapshot without an LFS artifact. An entirely missing object never reaches the existing checksum check. The repo::import function only performs clone --mirror, with no explicit LFS content import. Successful Git object import therefore does not establish complete LFS content.

After validating the file list, append_dir_all traverses the directory again, so the archive is not bound to the validated set. The walk_files helper uses is_dir(), which follows directory links, and is reused after extraction. Exploitability has not been verified; this assessment does not claim a vulnerability in the tar library itself. The application should explicitly reject unexpected entries and cyclic links.

Approach: extract an lfs module that binds pointer requirements, object presence/size/hash, archive entries, and restore verification to one snapshot. Include objects referenced only by historical commits in tests. Existing v1 snapshots need not be rewritten, but complete verification must identify missing content and avoid claiming full protection.

### D6 · P1: Repository creation, replacement, and identity lack a unified transaction

Confirmed by code inspection: `repo.rs:20` and `:30` initialize or clone directly into the final directory. A subsequent configure failure leaves a partially configured repository. Initial backup is separately invoked in main.rs, so library callers do not receive the same workflow as CLI callers.

The ordinary-error rollback in `restore.rs:137` is useful, but there is no recovery record if the process exits after moving the old directory aside and before publishing its replacement. After successful publication, failure to delete the old directory returns an ordinary error. A leftover `.refuge-replaced-*.git` also matches repo::list's .git filter and can appear as a normal repository.

In addition, restore --as preserves repo_id. If the original remains active, two repositories can share one ID, while resolve selects the first match. Writes from both enter the same backup namespace and confuse provenance and status. Preserve the existing identity-preservation test, but add a duplicate-identity policy.

Approach: stage create/import/restore consistently, keep recovery records in a separate directory, and make cleanup retryable after commit. Reject duplicate active repo_id values within one configuration by default. The --as option renames a restored repository; it does not implicitly create a new identity. A future fork capability should be a separate feature.

### D7 · P1: Application workflows, errors, and Git execution policies are scattered

Confirmed by code inspection: `main.rs:224` combines parsing, create/import → backup → clone/connect orchestration, and output. Restore depends on backup for checksum and LFS verification helpers. The core RefState value lives in the Git I/O module. Most layers return anyhow errors, while future retry logic will need reliable distinctions between corruption, conflicts, transient I/O, and environmental failures.

The git.rs module constructs Command::new in several places and converts some paths to UTF-8 while using OsString elsewhere. Working-directory discovery also occurs in repository library functions. The policy for Git environment variables inherited by hooks and subprocesses targeting other repositories is not explicit.

Approach: extract shared verification and application use cases, introduce a limited set of structured errors and outcomes, and retain anyhow context at the CLI boundary. Centralize the Git runner's cwd, arguments, exit-code handling, and environment policy. Add interfaces only at fault-injection or genuinely replaceable I/O boundaries; every function does not need a trait.

### D8 · P1: Regression infrastructure and platform gates are incomplete

Confirmed by code inspection: tests duplicate initialization, Git fixtures, PATH injection, and manifest discovery. There is no .github CI configuration. Cases for concurrent staging, invalid manifests, crash stages, and interrupted replacement are missing. Real LFS BDD is valuable, but dependency installation is not part of a reproducible test entry point.

Approach: share tests/support, keep BDD focused on user journeys, and cover formats, publication, and selection with fast tests. Retain real Git/LFS end-to-end coverage. Prioritize Linux and Windows gates; include macOS if the README continues to claim support.

### D9 · P2: Configuration contracts and scaling costs need consolidation

Config::save_to overwrites files directly. The init exists check does not protect against concurrent creation, and load_from does not revalidate path separation. OneDrive detection uses environment variables and path prefixes, which cannot establish the README's absolute claim that the default directory is never cloud-synced.

Each backup scans historical manifests for the next generation. Status checks all historical artifacts, UUID snapshot selection still scans all repositories, and resolve enumerates repositories while launching Git for each. Full LFS archives also grow with accumulated history. These are complexity risks without performance measurements; introducing a database or cache first would be premature.

Approach: save configuration atomically, validate it on load, and correct documented guarantees. Prefer direct UUID lookup and eliminating repeated scans. Establish benchmarks before deciding whether a rebuildable index is worthwhile. A cache must never become the sole evidence of protection.

## 4. Lessons from tgrep

Paths below are relative to the sibling ../tgrep checkout. Line numbers refer to b1d0fc2; locate symbols if the source changes.

| Reference implementation | Mechanism worth adopting | Refuge application and limits |
| --- | --- | --- |
| Cargo.toml; tgrep-core/src/error.rs | Core/CLI boundary and matchable error variants | Establish module boundaries first; no immediate workspace split or copying search-domain errors |
| PublishStatus and publish_staged_index in tgrep-cli/src/serve.rs:7759 | Separate publication and rollback failures; reopen published data for validation; preserve recovery material | Apply to SnapshotPublisher and RepositoryPublisher; do not copy mmap or per-file index replacement protocols |
| staged_publish_order and StagedFileMove in serve.rs:8042 | Centralized ordering, tracked rollback, Drop fallback | Preserve manifest-last publication; Drop handles stack unwinding, not process-crash recovery |
| BackupDir and cleanup_retired in tgrep-cli/src/serve/index_cleanup.rs | Exclusive creation, commit markers, retryable cleanup, rejection of links and unexpected contents | Manage staging and replaced repositories without recursively deleting arbitrary user directories |
| tgrep-core/tests/snapshot_consistency.rs | Query results stay bound to the reader generation that produced them | Bind refs, bundle, LFS, and manifest to one PreparedSnapshot; locking mechanisms differ |
| tgrep-core/tests/corrupt_index_reader.rs | Deterministic corrupt inputs test panic and work bounds without nightly tooling | Add malformed manifest, archive, and path regressions; fuzzing can follow later |
| .github/workflows/ci.yml | Formatting, Clippy, and real tests across platforms | Add Git/LFS prerequisites and restore drills; select action versions independently |

tgrep is not a complete architecture template: its serve.rs has 14,467 lines, showing that splitting core and CLI crates alone does not ensure focused responsibilities. Rebuildable indexes and irreplaceable backups also have different failure costs. tgrep can fall back to scanning source files; Refuge's original repository may no longer exist during restoration.

## 5. Target structure and contracts

Evolve toward these responsibilities task by task. Names may change; do not create all the empty modules upfront.

```text
src/main.rs                  Entry point and exit codes
src/cli/                     Clap definitions, dispatch, and presentation
src/application.rs           Create/import/backup/restore use cases and partial success
src/domain.rs                RefState, protection states, and limited domain value types
src/error.rs                 Decision-relevant errors retaining their original source
src/git.rs                   Real Git execution and output parsing
src/lfs.rs                   Pointer requirements, object checks, controlled archives
src/manifest.rs              v1 wire format, compatibility, and semantic validation
src/snapshot/                Catalog, verification levels, selection, protection state
src/storage/                 Layout, publication, locking, synchronization, recovery records
src/backup.rs                PreparedSnapshot-to-publication orchestration
src/restore.rs               Candidate verification, staged repository, publication
src/repo.rs                  Repository identity, resolution, connection, configuration
src/config.rs                Path policy, configuration loading, atomic saving
tests/support/               Isolated fixtures and real subprocess helpers
```

Dependency direction: CLI → application → domain/manifest + git/lfs/storage. Verification belongs in shared modules; restore should no longer obtain helpers through backup. Expose only types callers actually need, without prematurely stabilizing the entire Rust API for a future daemon.

Establish these contracts:

1. Each operation owns independent staging; each artifact belongs to one PreparedSnapshot.
2. The manifest is the commit marker. Failure cannot modify existing snapshots, and cleanup is separate from commit status.
3. Catalog returns records and diagnostics. Unparseable files do not require fabricated Manifest values. Unsupported and Corrupt are distinct.
4. Quick inspection establishes structure, presence, and size. Complete verification covers checksums, Git refs/hash, object integrity, and LFS requirements. Status and output explain verification scope without implicitly reading every large artifact on every status call.
5. Default restore tries verifiable candidates from newest to oldest; explicit selection fails strictly. Generation values across instances are not a reliable global timeline.
6. repo_id is unique within a configuration instance. Recovery-material directories are excluded from normal repository enumeration.
7. Application outcomes identify completed stages, such as creation succeeding before initial backup fails, and provide executable recovery commands. A failed backup must not trigger deletion of an already created repository that may contain user data.

Compatibility constraints: preserve existing CLI arguments, hook lookup of refuge through PATH, manifest v1 field meanings, and historical layouts. Ignore unknown additive fields; explicitly reject unsupported schemas and encryption. Old snapshots without lfs_artifact must remain parseable. Fixes must not bulk-rewrite historical manifests or silently delete orphans.

Document changes to protection and default restore fallback semantics in release notes, including implications for older tools. If mandatory fields or ordering/identity contracts must change, write a separate format proposal rather than embedding migration in a module-extraction commit.

## 6. Agent execution packages

Priority indicates risk; dependencies determine execution order. Each package needs a failing case, implementation, regression verification, and explanation. Lines moved are not an acceptance criterion.

### R0 · P1: Reproducible test entry point and shared fixtures

Scope: tests/support, test helpers, CI, and development instructions. Preserve behavior assertions and avoid rewriting every test at once. Correct the two existing formatting differences, then establish Linux/Windows formatting, Clippy, and test checks. CI must explicitly install and check Git LFS. Release gates must not silently skip LFS BDD when dependencies are missing.

Acceptance: the full suite passes with prerequisites installed. Fixtures isolate configuration, HOME/XDG, and Git global/system configuration for each subprocess, explicitly setting user identity, branch, and hook PATH. Do not modify the developer's real home environment or global Git configuration. Any git-lfs-specific configuration is confined to fixtures. Preserve real hook-driven push tests and push-after-restore tests.

Dependencies: none. Size: small to medium.

### R1 · P0: Isolated staging and deterministic cross-repository concurrency coverage

Scope: backup.rs and concurrency tests. Initially leave the rest of publication unchanged for reviewability.

Acceptance: fixed time or controlled ID generation plus barriers make two repositories back up the same generation concurrently. Verify each bundle's refs and LFS content independently. Two processes backing up one repository remain serialized. Failure cleans only the current operation's materials, leaving other writers untouched. Do not rely on probabilistically landing in the same second.

Dependencies: none; this can ship before R0. Size: small.

### R2 · P0: Manifest validation and a fault-tolerant catalog

Scope: manifest.rs, discovery.rs, limited domain errors, and the reading policy in next_generation.

Implementation: centralize layout/key parsing. Validate schema, artifact format/version, encryption support, repo_id/filename consistency, refs/hash, and empty-repository rules while retaining unknown-field compatibility. Bound manifest input size and diagnose exceeded limits. Isolate entry failures in the catalog and route UUID queries directly to their directory.

Compute generation only from trustworthy records. Report corrupt records; do not silently treat unknown formats or other writers as absent. If safe write ordering cannot be determined, stop writing and report a conflict. Final-path collisions must reject overwrite independently of generation calculation.

Acceptance: broken JSON does not block unrelated repositories; an explicitly selected corrupt file produces a diagnostic; unknown schema/encryption cannot enter restoration; old manifests lacking LFS fields remain readable; mismatched refs/hash or filename are rejected. Test absolute paths, .., Windows prefixes and separators, and symbolic-link escape. Linux Path component checks alone do not establish platform compatibility.

Dependencies: reuse R0 fixtures where available; complete CI is not a prerequisite. Size: medium.

### R3 · P0: File publication component and explicit commit outcomes

Scope: storage, backup publication, and fault-injection tests. Implement a concrete FsSnapshotStore first, with narrow interfaces for injecting write/sync/rename failures. Graph and S3 are out of scope.

Implementation: unique partial files on the destination volume, checksum/size verification after copying, explicit synchronization errors, and manifest commit only after successful artifact publication. Test durability policy per platform. Unsupported durability guarantees must produce visible degradation rather than unconditional Protected status. Cleanup failure after completion returns a warning.

Acceptance: inject failures during copying, artifact flush/rename, and manifest write/flush/rename. Existing snapshots remain unchanged. No new complete snapshot is discoverable before commit; committed snapshots restore successfully. Destination corruption preserving file size is caught before publication. Materials remain identifiable after process termination, and cleanup acts only on confirmed operation-owned materials that are not in use.

Limitations: process-kill tests establish process-crash behavior, not physical power-loss durability. Ordinary rename is not a portable atomic no-overwrite operation. Define and test final-publication conflict handling.

Dependencies: R1 and R2. Size: medium to large.

### R4 · P0: Complete verification and restore candidate fallback

Scope: shared snapshot verification, restore.rs, an fsck wrapper in git.rs, and status documentation.

Implementation: in isolated staging, perform checksum → bundle verify → mirror clone → HEAD restoration → refs/hash comparison → fsck before repository publication. Use the same controlled artifact copy after validation so a sync client cannot replace it between verification and clone. Collect structured diagnostics during default fallback.

Acceptance: same-size corruption, missing artifacts, and invalid JSON in the newest snapshot do not prevent default restoration of an older valid snapshot. An explicitly selected corrupt ID fails without creating the destination. If all candidates fail, return their reasons. Disk exhaustion and permission errors do not silently select older data. Reject manifest refs that differ from the bundle. An older restored snapshot can be backed up again. Display the actual restored ID and fallback notice.

Dependencies: R2 and R3. Incorporate LFS completeness verification after R5; until then, do not describe acceptance as complete protection of all data. Size: medium.

### R5 · P0: LFS completeness and controlled archive handling

Scope: new lfs.rs, backup/restore callers, and LFS import failure semantics. A new LFS network service is out of scope.

Implementation: derive canonical pointer oid/size requirements from all history reachable in the selected snapshot. Validate that required objects are complete. Archive an explicit file list and validate actual archived content rather than traversing independently twice. Reject symbolic links, hard links, special devices, and escaping entries. Define configurable entry-count and expanded-size limits with explicit errors; never silently truncate backups.

Initially preserve the existing behavior of archiving present objects while adding required-object completeness checks. Do not remove unreferenced LFS objects in this task. If an import source contains pointers but hosted objects are missing, report incomplete protection and recovery instructions. Reuse the verifier for any local object copying. Automatic network fetching is a separate feature.

Acceptance: cover a missing object, a missing entire object directory, objects referenced only in historical commits, oid/size mismatch, changes during archiving, cyclic links, and nonregular archive entries. None may produce false full-protection results. A real git-lfs push → backup → clean-machine restore → pull round trip must reproduce original content. Supplying previously missing required objects must allow a new backup and updated completeness results even when refs are unchanged.

Dependencies: R1 and R2. Coordinate the shared PreparedSnapshot contract and merge order with R3/R4. Size: large; split requirement validation and archive handling into separate commit groups.

### R6 · P1: Repository lifecycle and replacement recovery

Scope: repo.rs, restore.rs, repository publication, and recovery records. A daemon is out of scope.

Implementation: configure create/import repositories in staging before publication. Backup failure reports that the repository exists but its backup is incomplete, with a manual backup command. Replacement writes a recoverable operation record and preserves the previous repository until commit completes. Defer and retry cleanup, keeping internal directories outside normal enumeration. Standardize lock ordering for Refuge mutations, covering both destination names and affected identities to prevent competing publication under one name.

Native Git pushes do not automatically honor Refuge application locks. Replacing an existing repository requires a maintenance exclusion mechanism or detection and rejection of active use. Until that constraint is implemented, do not claim --replace is safe concurrently with pushes.

Acceptance: configure failure leaves no partially configured repository under its final name. Process termination at each replacement stage permits recovery to an identifiable old or new state. Cleanup failure does not report completed restoration as failed, and the retained old copy is locatable. Old copies do not appear in repo list. Reject duplicate active repo_id values and ambiguous ID resolution instead of selecting the first match. Never overwrite an existing destination without --replace.

Dependencies: R2, R3, and R4. Size: large; implement staged create/import before replacement recovery.

### R7 · P1: Consolidated application workflows, output, and Git runner

Scope: main/cli, application, git, and domain/error. Extract around the outcomes established by R1–R6 rather than designing a generic framework first.

Implementation: CLI parses, invokes application services, and renders outcomes. Application use cases own initial backup for create/import. Move checksum and LFS helpers out of backup. Use OsStr/OsString for Git arguments and explicit protocols for textual output. Pass cwd from callers. Define inherited-environment policy for repository-location variables, exercise it through real hooks, and preserve legitimate authentication/configuration needs.

Acceptance: library and CLI invoke the same creation/import workflows. Existing commands, help, and PATH hook contracts remain intact. Partial success does not require string parsing. Preserve Git exit status and stderr. Test paths containing spaces and Unicode on Windows/Linux; if Unix non-UTF-8 paths are unsupported, report that limitation consistently. Avoid unnecessary async runtimes, dependency-injection containers, or seven-crate splits.

Dependencies: stabilized R2–R6 interfaces. Size: medium.

### R8 · P2: Configuration robustness, performance baseline, and aligned documentation

Scope: configuration, snapshot queries, benchmark scripts, README, manifest-v1, and older roadmap documents.

Acceptance: concurrent init cannot overwrite instance identity; interrupted writes cannot corrupt existing configuration; loading checks path rules. Document sync-directory detection limits. Measure status/list/backup duration, subprocess count, and I/O using multiple repositories, many snapshots, and LFS fixtures before choosing cache/index optimizations. Do not invent unmeasured latency thresholds.

Document LFS artifacts, verification levels, durability guarantees, and writer/generation scope in manifest-v1. Mark the older implementation plan as a long-term reference. A future daemon should invoke existing backup use cases, treating spool entries as wake-up signals and reconciliation as compensation. Introduce a queue or SQLite when implementing durable retry.

Dependencies: correctness tasks first; performance changes must preserve the snapshot source of truth. Size: medium.

## 7. Execution order and handoff requirements

Suggested order: R1 immediate fix → R0 baseline → R2 → R3 → R4/R5 → R6 → R7 → R8. R0 and R1 have no hard dependency on each other. R4/R5 can be implemented separately, but changes to shared callers need coordinated merge review.

For a limited first round, complete the correctness work in R0–R5 and defer broad CLI movement and performance optimization. Directory reorganization alone is not sufficient preparation for a daemon: asynchronous retry needs reliable knowledge of whether an operation committed.

Each implementation agent should:

1. Identify the task, dependency baseline, and behavior changes. Read this document and current code before acting; line numbers may have moved.
2. Reproduce failures deterministically. Use barriers or injection points for concurrency; do not assume chmod must fail when tests run as root.
3. Stay within the task boundary. Separate mechanical moves from semantic fixes where practical and avoid repository-wide rewrites.
4. Report validation commands, actual results, and untested platforms. Format changes require old-fixture compatibility tests.
5. Explain changed error assertions through the new contract; deleting a failing test is not a fix.
6. If authorized to commit, use independently reversible units. This plan does not require pushing, opening PRs, deployment, or deletion of real backups.
7. Keep all repository documentation, comments, and other written project content in English.

Shared completion criteria: full Git/LFS tests and static checks pass; key publication and restore drills pass on Windows/Linux; old v1 snapshots remain restorable; failures preserve existing published snapshots; cleanup is retryable and does not change commit outcomes; README guarantees match implemented behavior.

## 8. Official references and applicability

These sources support specific mechanisms rather than establishing one implementation as universally appropriate:

- [SQLite Atomic Commit](https://www.sqlite.org/atomiccommit.html): explains why commit markers, flush ordering, and recovery must be considered together. Apply the transaction principles without introducing SQLite solely for file publication.
- [Rust std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html): documents platform and filesystem considerations; rename is not a general no-clobber transaction API.
- [Git bundle](https://git-scm.com/docs/git-bundle): verify checks bundle format and application prerequisites; list-heads supports ref comparison.
- [Git fsck](https://git-scm.com/docs/git-fsck): checks object database validity and connectivity as a separate restore verification step.
- [Git LFS pointer specification](https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md): pointers carry oid and size. This motivates validating required snapshot objects rather than only objects present on disk.
- [Git hooks](https://git-scm.com/docs/githooks): hook working directories and Git environment variables affect subprocess repository selection. Refs are already updated when post-receive runs, so a backup lock does not block pushes.

Architecture choices and priorities are engineering judgments for Refuge's current implementation and next iterations. Do not assume tgrep has solved backup-specific concerns such as multi-machine synchronization, LFS completeness, or power-loss durability.
