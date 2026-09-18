---
title: Refuge Product Requirements Document
status: Draft
date: 2026-09-18
outline: |
  - L1 Provide a lightweight Git host with recoverable off-site backups
    - L2 Support local-first and server deployment modes through the same product
      - L3 Keep work data on one compliant machine or serve personal repositories to multiple devices
    - L2 Treat the Git host as primary storage and remote storage as read-only backup
      - L3 Back up every successful push asynchronously as a verified, immutable snapshot
    - L2 Make disaster recovery a first-class workflow
      - L3 Rebuild a lost Git host on a clean machine using only remote storage and optional credentials
---

# Refuge Product Requirements Document

## 1. Document purpose

This document defines the product requirements for **Refuge**, a lightweight Git hosting and disaster-recovery product.

It is written for the engineers and agents that will design and implement the product. It describes the user problem, product boundaries, required behavior, failure semantics, and acceptance criteria. It intentionally does not prescribe a programming language, framework, database, or detailed software architecture.

### 1.1 Product name and identity

The product name is **Refuge**.

The name combines two ideas:

- **Ref:** The Git references, such as branches and tags, that describe repository state.
- **Refuge:** A safe place that protects something valuable and provides a path to recovery.

The name reflects the product's purpose: providing a safe, recoverable home for Git history without requiring a public collaboration platform.

The product tagline is:

> **Refuge — A safe home for your Git history.**

The canonical product name is `Refuge`. The canonical CLI command should be `refuge`, subject to package, trademark, and command-name availability checks before public release. Documentation, interfaces, artifact metadata, and user-facing messages must not use the former working name `Git Vault`.

## 2. Executive summary

Refuge is a local-first, self-hosted Git service that:

1. Accepts standard Git push and pull operations.
2. Stores the live repository on a machine controlled by the user.
3. Creates a verified repository snapshot immediately after every successful push.
4. Publishes that snapshot to a configurable remote storage provider, starting with OneDrive.
5. Can reconstruct the Git service on a clean machine when the original host is lost.

Refuge supports two deployment modes:

- **Local-first mode:** Runs on one machine and serves repositories only to that machine. This is intended for work environments where data must remain on company-managed devices and storage.
- **Server mode:** Runs on a private server and serves repositories to multiple authorized devices. This is intended primarily for personal use.

The two modes use the same repository, backup, verification, and recovery model. They differ only in network exposure, authentication requirements, and deployment topology.

Refuge is not a GitHub replacement. It is a Git-native storage service with disaster recovery. It does not provide pull requests, issues, code review, CI/CD, project management, or social collaboration.

## 3. Background

### 3.1 Current user workflow

The initial use cases are private or work-related projects that benefit from Git version control but should not be placed on a third-party Git hosting platform.

Examples include:

- A personal Beancount repository containing financial records.
- A private Obsidian vault.
- Work repositories that may only be stored on company-managed computers and company-provided OneDrive accounts.

These projects often generate short-lived or machine-specific files such as:

- `__pycache__`
- Python virtual environments
- Editor caches
- Generated files
- Temporary importer output

OneDrive does not provide a repository-level equivalent to `.gitignore`. Placing the working directory directly in OneDrive therefore causes unnecessary synchronization, conflict noise, and wasted storage activity.

### 3.2 Existing workaround

An existing workaround is to place a bare Git repository in OneDrive and create a Git worktree on a non-synchronized local disk.

This separates the working tree from OneDrive and prevents temporary files from being synchronized. However, it exposes the internal files of an active Git repository to a general-purpose file synchronization system. This creates several concerns:

- Git internal state changes while OneDrive is observing and uploading files.
- Partial synchronization or conflict copies can produce an inconsistent remote copy.
- Multiple machines may write incompatible states into the same synchronized repository.
- Git worktree metadata may contain machine-specific paths.
- Upload success does not prove that the synchronized repository is restorable.
- Recovery behavior is implicit and has no verification, status, or guided workflow.

### 3.3 Core product contradiction

Users need Git's version-control and multi-device capabilities, but:

- Private or regulated data cannot always be uploaded to a public or personal Git hosting service.
- The cloud storage that users are allowed to use is not suitable for hosting an actively changing Git repository.

Refuge resolves this by separating responsibilities:

- The Git host owns the live, writable repository.
- Remote storage receives immutable, verified backup artifacts.

## 4. Product vision

> Refuge is a lightweight Git host that turns every successful push into a recoverable off-site backup without requiring users to place an active repository in a synchronized folder or use a third-party Git collaboration platform.

The product should make these statements true:

- A developer uses ordinary Git commands and does not need to understand the backup implementation.
- The live repository is never directly operated from OneDrive or another backup provider.
- Temporary working-tree files never enter the backup unless they are committed.
- A storage outage does not prevent local Git work.
- A lost Refuge host can be reconstructed from remote storage on a clean machine.
- Backup artifacts remain usable with standard Git tooling and do not depend on a proprietary repository format.

## 5. Product principles

### 5.1 Git-native

Clients must use standard Git concepts and transports. Refuge must not require a custom version-control client or a proprietary repository format.

### 5.2 Local ownership

The user chooses where the live Git host runs and where backup data is stored. Work data must be able to remain entirely within company-managed infrastructure.

### 5.3 One writable source of truth

The Git host is the primary, writable source of truth. Remote storage is a read-only backup destination from the product's perspective.

Remote backup artifacts must never be edited in place or treated as a second writable Git remote.

### 5.4 Push availability is independent of backup availability

A temporary OneDrive, S3, network, or authentication outage must not prevent a push after the Git host has safely persisted it locally.

Backup begins immediately after a successful push but runs asynchronously. The system must expose when the latest push has not yet been backed up.

### 5.5 Recovery is part of backup

A backup is not considered healthy merely because an upload request succeeded. The product must verify snapshot creation, artifact integrity, remote publication, and the ability to reconstruct a repository.

### 5.6 Safe defaults

The product must prefer retaining excess backup data over deleting recoverable history. Destructive retention behavior must be explicit and observable.

### 5.7 Narrow product scope

Refuge provides repository hosting, backup, and recovery. It must not expand into a general collaboration platform.

## 6. Target users and scenarios

### 6.1 Persona A: employee using a company-managed computer

The user:

- Works on one company-managed computer.
- Cannot upload work data to a personal server or third-party Git account.
- Can use company-provided local storage and OneDrive.
- Wants Git history without synchronizing caches and temporary files.
- Needs a recoverable copy if the computer is replaced or its disk fails.

Expected configuration:

- Local-first mode.
- No externally reachable Git service.
- Live repositories stored outside OneDrive.
- Backup destination is company OneDrive.
- All data remains within company-approved boundaries.

### 6.2 Persona B: individual using multiple personal devices

The user:

- Owns a private server, NAS, or always-on computer.
- Uses several personal devices.
- Wants those devices to push and pull from a private Git host.
- Does not need GitHub-style collaboration features.
- Wants the host backed up to OneDrive or another storage provider.

Expected configuration:

- Server mode.
- Authenticated access from authorized devices.
- Live repositories stored on the private server.
- Backup destination is personal OneDrive initially, with additional providers later.

### 6.3 Disaster-recovery scenario

The Refuge host is lost, replaced, corrupted, or reinstalled. The user installs Refuge on a clean machine, connects it to remote storage, selects a repository snapshot, and reconstructs a working Git host without relying on metadata from the lost machine.

## 7. Scope

### 7.1 MVP scope

The MVP must provide:

- Creation or import of bare Git repositories.
- Standard Git push and pull.
- Local-first deployment mode.
- Server deployment mode.
- Immediate asynchronous backup after successful pushes.
- Full repository snapshots using a Git-compatible artifact such as `git bundle`.
- Snapshot verification before publication.
- OneDrive as the first remote storage provider.
- Durable retry of failed backups.
- Repository-level backup status.
- Discovery of repositories and snapshots from remote storage.
- Restore of a repository onto a clean Refuge installation.
- Optional client-side encryption using a user-provided master passphrase, if encryption is enabled for a repository or storage target.
- A command-line or similarly automatable administration interface.

### 7.2 Explicitly out of scope

The following are not part of the product:

- Pull requests or merge requests.
- Issues, discussions, wikis, or project boards.
- Browser-based source code review.
- CI/CD pipelines.
- Package or container registries.
- Source-code search.
- General file synchronization.
- Backup of uncommitted working-tree changes.
- Automatic commit creation.
- Merge conflict resolution.
- Bidirectional synchronization with OneDrive.
- Editing backup artifacts in remote storage.
- Multi-tenant organization management.
- Enterprise role-based access control.
- Replacing company policy, data classification, or compliance review.

Server mode may support multiple devices, but the MVP is a single-owner service rather than a team collaboration platform.

## 8. Conceptual architecture

Refuge has three logical areas.

### 8.1 Git service layer

Responsibilities:

- Own the live, writable bare Git repositories.
- Accept standard Git push and pull operations.
- Persist Git updates safely.
- Notify the backup coordinator after a successful push.
- Enforce deployment-mode-specific access restrictions.

### 8.2 Backup storage layer

Responsibilities:

- Store immutable repository snapshots.
- Support upload, download, listing, integrity metadata, and safe deletion.
- Hide provider-specific behavior behind a storage adapter.
- Begin with OneDrive and permit future providers such as S3-compatible object storage.

### 8.3 Control and recovery layer

Responsibilities:

- Maintain repository identity independently from a machine-specific filesystem path.
- Queue backup jobs durably.
- Track backup state and retry failures.
- Create and verify snapshots.
- Publish self-describing manifests.
- Apply retention policies.
- Discover remote snapshots without relying on the original host.
- Restore repositories on a clean installation.
- Manage optional encryption metadata without storing the master passphrase.

The control layer may use a local database for operational state, but remote recovery must not depend on that database surviving.

## 9. Deployment modes

### 9.1 Local-first mode

Local-first mode is optimized for a single machine.

Requirements:

- The service must not expose repositories to other machines by default.
- If a network listener is used, it must bind only to loopback by default.
- A filesystem-based Git remote may be used where appropriate.
- The product must run without requiring a public DNS name, TLS certificate, or external identity provider.
- The background backup process must start automatically or be easy to run reliably through the operating system.
- Repositories and temporary snapshot files must be stored outside the OneDrive synchronized directory.
- The only files placed in OneDrive must be published backup artifacts and their metadata.

### 9.2 Server mode

Server mode is optimized for a privately operated, multi-device Git host.

Requirements:

- Multiple authorized devices must be able to push and pull.
- Access must use a standard Git-compatible transport.
- Anonymous write access must not be allowed.
- Authentication credentials must not be stored in repository backup artifacts.
- Backup behavior and artifact format must remain the same as local-first mode.
- The server must continue serving repositories during a temporary storage-provider outage.

The choice of SSH, smart HTTP, or both is an implementation design decision. The selected transport must work with standard Git clients and provide an appropriate authentication mechanism.

## 10. Repository lifecycle requirements

### 10.1 Repository identity

Each repository must have:

- A stable, globally unique internal identifier.
- A human-readable name.
- A live repository location.
- A configured backup target.
- A backup policy.
- An encryption policy.

Repository identity must not depend solely on a local absolute path because that path may change during recovery.

### 10.2 Repository creation

The user must be able to create an empty hosted repository and receive instructions for adding it as a Git remote.

### 10.3 Repository import

The user must be able to import an existing local Git repository or bare repository without rewriting its history.

Import must preserve:

- Branches.
- Tags.
- Git notes and other supported refs where practical.
- Commit and object identities.

### 10.4 Push and pull

The Git service must support ordinary clone, fetch, pull, and push workflows using standard Git clients.

A push is successful when the Git host has durably accepted the Git ref updates. Remote backup completion is not part of the push transaction.

### 10.5 Repository deletion

Deletion of a live repository and deletion of its remote backups are separate operations.

The MVP must not automatically delete remote backups when a live repository is removed. Any permanent remote deletion must require an explicit administrative action and clear confirmation.

## 11. Backup requirements

### 11.1 Backup trigger

Every successful push must enqueue a backup of the resulting repository state.

The enqueue operation must:

- Occur only after the Git update succeeds.
- Be durable across a Refuge process restart.
- Not block on remote storage availability.
- Preserve enough information to determine which repository generation needs backup.

The worker should begin processing promptly. Under normal operating conditions, a backup attempt should begin within 10 seconds after a successful push.

### 11.2 Push coalescing

Several pushes that arrive while a backup is pending may be coalesced into one snapshot of the newest repository state.

Coalescing is valid only when:

- The final snapshot contains the state of the newest accepted push.
- The product does not incorrectly report an earlier push as backed up before a snapshot covering it is published.
- The user can determine whether the current repository state is protected remotely.

The product is not required to preserve one backup artifact per push.

### 11.3 Snapshot contents

An MVP snapshot must be sufficient to reconstruct the hosted bare repository, including all refs selected by the product's repository policy.

By default, the snapshot should include all relevant refs rather than only the default branch. This normally includes branches, tags, and other intentional refs.

Working-tree files that have not been committed are not included.

### 11.4 Snapshot format

The snapshot must use a Git-compatible, documented format. A full `git bundle` is the preferred MVP representation because it:

- Encapsulates Git objects and refs in one file.
- Can be verified with standard Git.
- Can be cloned or fetched without Refuge.
- Avoids synchronizing a live repository's internal files.

The MVP should favor complete, independently restorable bundles over incremental bundle chains. Incremental backups may be introduced later if repository size or bandwidth requires them.

### 11.5 Snapshot consistency

The snapshot must represent a coherent set of refs and objects.

Concurrent pushes and background Git maintenance must not produce a published snapshot that references missing objects or mixes incompatible repository states.

The implementation must define how it obtains a stable repository view while minimizing disruption to push and pull operations.

### 11.6 Verification

Before publication, every snapshot must be verified.

Verification must detect at least:

- Invalid bundle structure.
- Missing prerequisites that would prevent standalone recovery.
- Missing or inconsistent Git objects.
- Local read or write failures.

The product must calculate and record a cryptographic checksum for the final artifact.

### 11.7 Publication semantics

Remote consumers must never treat a partially uploaded artifact as a valid backup.

Publication must use one of these patterns:

- Upload to a temporary object and atomically promote or rename it.
- Upload the artifact first and publish its manifest only after the artifact is complete.
- Use another provider-specific mechanism with equivalent visibility guarantees.

A backup is complete only after:

1. Snapshot creation succeeds.
2. Local verification succeeds.
3. Artifact upload succeeds.
4. Required metadata is published.
5. The remotely stored artifact can be identified and its size or checksum confirmed.

### 11.8 Backup manifest

Every published snapshot must have self-describing metadata containing at least:

- Manifest schema version.
- Stable repository identifier.
- Repository name at backup time.
- Snapshot identifier.
- Creation timestamp in UTC.
- Source Git generation or sequence number.
- Included refs and their object IDs, or a reference to equivalent verifiable metadata.
- Artifact filename or object key.
- Artifact byte size.
- Artifact checksum and checksum algorithm.
- Snapshot format and format version.
- Encryption status and encryption metadata version.
- Refuge product version that created the snapshot.

Manifests must be sufficient to discover and evaluate backups from a clean installation.

### 11.9 Immutability

Published snapshot artifacts must not be modified in place.

If metadata must be corrected, the product must publish a replacement manifest or snapshot with a new identity rather than silently changing a previously verified artifact.

### 11.10 Backup states

At minimum, each repository must expose these states:

- **Protected:** The latest accepted repository generation has a verified remote snapshot.
- **Pending:** A newer accepted generation is queued or currently uploading.
- **Degraded:** Backup of the latest generation has failed but will be retried.
- **Unprotected:** No valid remote snapshot is known.
- **Unknown:** The product cannot currently determine remote protection status.

The state must not report `Protected` merely because a local bundle exists.

### 11.11 Retry behavior

Transient failures must be retried automatically using bounded exponential backoff with jitter or an equivalent strategy.

Requirements:

- Retry state must survive process restarts.
- Authentication failures must be distinguished from transient network failures.
- Permanent or prolonged failures must be visible to the user.
- A newer push must not permanently strand or hide an older failed queue item.
- Repeated failures must not create unbounded temporary files.

### 11.12 Retention

The default MVP behavior must not silently delete the only valid backup of a repository.

If automatic retention is implemented:

- It must be configurable per repository or backup target.
- It must never delete the newest verified snapshot.
- It must not delete an artifact required by another snapshot.
- Deletion failures must be reported separately from backup failures.
- The product should publish the new snapshot successfully before deleting old snapshots.

The safest MVP default is to retain all snapshots and make storage consumption visible. A later default may use a documented policy such as recent, daily, weekly, and monthly retention.

## 12. Storage provider requirements

### 12.1 Storage adapter contract

Each storage adapter must support the operations required for:

- Authentication or connection setup.
- Uploading an artifact.
- Publishing an artifact safely.
- Uploading and publishing a manifest.
- Listing repository manifests.
- Downloading artifacts and manifests.
- Reading artifact metadata such as size or provider checksum.
- Deleting specific artifacts when explicitly requested by retention.
- Reporting provider-specific errors in a provider-independent error model.

The adapter boundary must not expose Git repository logic.

### 12.2 OneDrive provider

OneDrive is the MVP storage provider.

The product may support OneDrive through:

- A local OneDrive synchronized folder.
- Microsoft Graph.
- A mature external transport such as rclone.

The implementation must choose and document one primary MVP approach. Regardless of approach:

- Active bare repositories must not reside in the synchronized folder.
- Temporary snapshot creation must occur outside the synchronized folder.
- Partially written files must not appear as valid backups.
- Account or tenant information must be visible enough for the user to avoid backing up work data to a personal account.
- Provider throttling and temporary offline behavior must not cause data loss.

### 12.3 Future providers

The design must allow additional providers without changing Git service behavior or snapshot semantics.

Expected future providers include:

- S3 and S3-compatible object storage.
- Local or network filesystem targets.
- NAS storage.
- Other cloud drives.

Provider support does not imply that all providers have identical atomic rename, checksum, or consistency behavior. The adapter must implement equivalent safe-publication semantics.

## 13. Encryption requirements

### 13.1 Product behavior

Client-side encryption is optional per backup target or repository.

When enabled:

- Snapshot content must be encrypted before it leaves the Refuge host.
- The remote storage provider must not receive plaintext repository content.
- The user supplies a master passphrase during configuration or recovery.
- Refuge must not upload the passphrase.
- Refuge must not store the passphrase in plaintext.
- Loss of the passphrase means the backup cannot be recovered.
- This consequence must be communicated clearly during setup.

### 13.2 Cryptographic design constraints

The implementation must not invent a new cryptographic algorithm or unaudited container format.

It must use a mature cryptographic library or established encrypted-backup format that provides:

- A password-based key derivation function intended for password storage or encryption.
- A unique random salt.
- Authenticated encryption that detects tampering.
- Independent nonces or equivalent misuse-resistant behavior.
- Versioned encryption metadata.
- A migration path for changing cryptographic parameters.

The product may derive a key-encryption key from the passphrase and use it to protect a randomly generated data-encryption key. The exact construction belongs in the technical design and security review.

### 13.3 Passphrase changes

The design should allow a user to change the master passphrase without rewriting all historical repository data where the selected encryption format permits this safely.

This is desirable but not required for the first implementation.

### 13.4 Recovery ergonomics

The recovery workflow must be able to:

- Detect whether a snapshot is encrypted.
- Request the passphrase interactively or through an approved secret-input mechanism.
- Distinguish an incorrect passphrase from a corrupt artifact where the encryption format permits.
- Avoid printing secrets in commands, logs, process arguments, or error messages.

## 14. Recovery requirements

### 14.1 Recovery objective

The defining recovery requirement is:

> When the original Refuge machine and all of its local operational data are unavailable, a user can install Refuge on a clean machine, connect to remote storage, and reconstruct a usable Git repository.

### 14.2 Discovery

After connecting to a backup target, the user must be able to list:

- Repositories present in that target.
- Available snapshots for each repository.
- Snapshot creation times.
- Included Git refs or at least the default branch head.
- Verification status.
- Encryption status.
- Product and manifest versions.

This discovery must use remote manifests and must not require the original local database.

### 14.3 Snapshot selection

The default recovery option should select the newest valid, compatible snapshot.

The user must also be able to select an older snapshot.

If the newest snapshot is corrupt, incomplete, unavailable, or unsupported, the product must:

- Report the reason.
- Preserve the failed artifact for investigation.
- Offer the newest older snapshot that passes validation.

### 14.4 Restore procedure

The restore operation must:

1. Download the selected manifest and artifact.
2. Confirm artifact size and checksum.
3. Request credentials or passphrase when needed.
4. Decrypt the artifact when needed.
5. Verify the Git snapshot.
6. Reconstruct a bare repository.
7. Verify the reconstructed refs and objects.
8. Register the repository with the new Refuge instance.
9. Make it available for clone, fetch, and push.
10. Avoid overwriting an existing repository unless the user explicitly approves it.

### 14.5 Recovery result

A successful recovery must produce a repository that:

- Can be cloned using a standard Git client.
- Contains the refs declared by the snapshot manifest.
- Passes an appropriate Git integrity check.
- Can accept a new push.
- Can produce a new verified remote backup.

### 14.6 Restore testing

The product must provide a non-destructive way to test a snapshot by restoring it into a temporary location and running integrity checks.

The system should support scheduled restore tests in a later release. For the MVP, an explicit administrative command is sufficient.

## 15. User experience requirements

### 15.1 Administration surface

The MVP may be CLI-first, but all critical workflows must be scriptable and return meaningful exit codes.

Required workflows:

- Initialize Refuge.
- Select local-first or server mode.
- Add and validate a OneDrive backup target.
- Create or import a repository.
- Obtain the Git remote URL or path.
- Enable or disable optional encryption.
- View repository protection status.
- Trigger a backup manually.
- Retry a failed backup.
- List remote snapshots.
- Test a snapshot.
- Restore a repository.
- View actionable diagnostics.

Exact command names are an implementation decision.

### 15.2 Status communication

For each repository, the user must be able to answer:

- What is the latest accepted Git state?
- Has that state been backed up?
- When was the last verified remote backup?
- Is a backup pending or failing?
- Which storage target holds the backup?
- Is encryption enabled?
- What action should be taken if the state is degraded?

### 15.3 Errors

Errors must be explicit and actionable.

Examples:

- `OneDrive authentication expired; reconnect the backup target.`
- `Snapshot was created but failed verification; no remote artifact was published.`
- `Upload completed but the remote checksum does not match; retry scheduled.`
- `Latest snapshot is corrupt; recovery can continue from snapshot <id>.`

The product must not convert failures into success-shaped fallback behavior.

## 16. Security and compliance requirements

### 16.1 Local-first isolation

Local-first mode must default to no external network exposure.

The user must deliberately opt in before another machine can access a repository.

### 16.2 Server authentication

Server mode must authenticate write access and should authenticate read access by default.

The MVP does not require organization-level identity management, but it must support revoking an individual device or credential without rewriting repository history.

### 16.3 Secrets

Credentials, access tokens, and passphrases must:

- Never be committed into hosted repositories.
- Never be included in bundle manifests.
- Never appear in ordinary logs.
- Be stored only through an operating-system credential store, approved secret store, or explicit runtime input.

### 16.4 Work-account safety

The product must clearly identify the configured OneDrive account or tenant before enabling a backup target.

This reduces the risk of accidentally sending company data to a personal OneDrive account. Refuge cannot determine company policy automatically, so final compliance remains the user's or administrator's responsibility.

### 16.5 Auditability

The product must record operational events including:

- Successful and failed pushes at a repository and timestamp level.
- Backup job creation.
- Snapshot verification.
- Remote publication.
- Retry and terminal failure.
- Restore attempts and results.
- Retention deletions.

Logs must not contain repository contents or secrets by default.

## 17. Reliability requirements

### 17.1 Process crashes

A process crash during snapshot creation or upload must not:

- Damage the live bare repository.
- Publish a partial snapshot as valid.
- Lose the fact that the latest repository generation requires backup.

Temporary files from interrupted jobs must be detected and cleaned up safely.

### 17.2 Machine restart

After restart, Refuge must:

- Reopen hosted repositories.
- Recover durable pending jobs.
- Reconcile interrupted jobs.
- Resume backup attempts automatically.
- Recompute protection state if local state is uncertain.

### 17.3 Storage outage

During a storage outage:

- Push and pull must continue while local storage remains healthy.
- Backup jobs must remain queued.
- The user must see a degraded or pending state.
- Jobs must retry after connectivity returns.

### 17.4 Local disk exhaustion

The product must detect insufficient local space before or during snapshot creation and report it distinctly from provider failures.

It must not delete the live repository or unverified backups automatically to recover space.

### 17.5 Remote corruption

If an artifact differs from its manifest checksum or fails Git verification:

- It must not be offered as the default valid recovery point.
- The failure must be visible.
- Older valid snapshots must remain available.
- The product must not silently replace historical evidence of corruption.

## 18. Performance and scale assumptions

The MVP is optimized for personal repositories and small-to-medium work repositories rather than very large monorepos or binary asset archives.

Initial assumptions:

- One owner per Refuge instance.
- A modest number of repositories.
- Push frequency typical of individual development.
- Full independently restorable bundles are acceptable.
- Backup may consume bandwidth in the background.

The implementation must avoid blocking Git operations on remote upload latency.

Metrics should be collected for:

- Snapshot creation duration.
- Snapshot size.
- Upload duration.
- Retry count.
- Time from successful push to protected state.
- Restore duration.

Incremental bundles, content deduplication, partial clone, and large-repository optimization are future considerations rather than MVP requirements. Git LFS content (a repository's `lfs/objects` directory) is backed up and restored alongside the git bundle, since a snapshot that silently drops large-file content would violate the product's recovery guarantee for any repository that happens to use LFS.

## 19. Backup state model

The implementation should model repository generations explicitly.

A conceptual sequence is:

```text
Push accepted
    -> generation N persisted
    -> backup job for generation N queued
    -> snapshot created
    -> snapshot verified
    -> artifact uploaded
    -> manifest published
    -> generation N marked protected
```

Possible job states include:

```text
queued
creating
verifying
encrypting
uploading
publishing
protected
retry_wait
failed
superseded
```

The exact persistence model is an implementation decision, but state transitions must be idempotent. Retrying a job after a crash must not create ambiguous or conflicting published snapshots.

## 20. Key failure scenarios

| Scenario | Required behavior |
|---|---|
| OneDrive is offline during push | Accept the push locally, mark backup pending or degraded, and retry later |
| Refuge crashes while creating a bundle | Keep the repository intact, discard or reconcile the temporary artifact, and retry |
| Refuge crashes during upload | Never publish the partial upload as valid; resume or restart safely |
| Two pushes arrive quickly | Coalesce if useful, but protect the newest accepted state and report status accurately |
| The latest remote bundle is corrupt | Reject it during restore and offer an older valid snapshot |
| The local operational database is lost | Discover repositories and snapshots from remote manifests |
| The entire host is lost | Restore onto a clean machine from remote storage |
| The encryption passphrase is wrong | Fail explicitly without damaging or replacing remote data |
| The encryption passphrase is lost | Explain that encrypted backups are unrecoverable; no bypass exists |
| OneDrive account points to the wrong tenant | Make account identity visible before backup is enabled |
| Remote storage fills up | Preserve the local repository, expose the failure, and do not claim protection |
| A repository is deleted locally | Preserve remote backups unless explicitly deleted |

## 21. MVP acceptance criteria

The MVP is acceptable when all of the following are demonstrated.

### 21.1 Local-first workflow

1. Install Refuge on a clean supported workstation.
2. Configure local-first mode.
3. Create or import a repository outside OneDrive.
4. Add Refuge as a remote from a working repository.
5. Push a branch and tag successfully.
6. Confirm that push succeeds without waiting for OneDrive upload.
7. Confirm that a backup job begins automatically.
8. Confirm that a verified immutable artifact and manifest appear in OneDrive.
9. Confirm that temporary working-tree files are absent from the artifact unless committed.

### 21.2 Server workflow

1. Install Refuge on a private server.
2. Connect two authorized devices using a standard Git client.
3. Push from one device.
4. Fetch or pull the new state from the other device.
5. Reject an unauthenticated write attempt.
6. Confirm that the successful push produces the same backup format used by local-first mode.

### 21.3 Offline-storage workflow

1. Make OneDrive unavailable.
2. Push a new commit successfully.
3. Observe a pending or degraded backup state.
4. Restart Refuge.
5. Restore OneDrive availability.
6. Confirm that the queued generation is backed up and becomes protected without another push.

### 21.4 Clean-machine recovery

1. Start with a new machine that does not contain the original repository or Refuge operational database.
2. Configure access to the existing OneDrive backup target.
3. Discover backed-up repositories and snapshots.
4. Restore the latest valid snapshot.
5. Verify the reconstructed repository using Git integrity checks.
6. Clone the restored repository with a standard Git client.
7. Confirm that expected branches and tags exist.
8. Push a new commit.
9. Confirm that the restored host creates a new verified backup.

### 21.5 Corruption handling

1. Corrupt or truncate the newest backup artifact in a test environment.
2. Confirm that checksum or Git verification rejects it.
3. Confirm that Refuge does not report that snapshot as valid.
4. Restore successfully from the newest older valid snapshot.

### 21.6 Encryption workflow, when enabled

1. Configure a backup target with a master passphrase.
2. Push and publish a snapshot.
3. Confirm that plaintext Git content is not recoverable directly from the remote artifact.
4. Restore successfully on a clean machine using the correct passphrase.
5. Confirm that an incorrect passphrase fails explicitly.
6. Confirm that the passphrase is absent from manifests, logs, command history generated by the product, and remote storage.

## 22. Success measures

Initial product success should be evaluated through reliability rather than engagement metrics.

Primary measures:

- Percentage of accepted repository generations that reach `Protected`.
- Median and 95th-percentile time from push acceptance to protected state.
- Rate of snapshot verification failure.
- Rate of remote publication failure.
- Successful clean-machine restore rate.
- Time required to restore a repository.
- Number of cases where status incorrectly reports a repository as protected.

The most important invariant is:

> Refuge must never report the latest repository state as protected unless a verified, discoverable remote snapshot covering that state has been published.

## 23. Product decisions already made

The following decisions are part of this PRD and should not be reopened during initial implementation unless a technical impossibility is demonstrated:

- The Git host is primary storage.
- Remote storage is read-only backup storage.
- Backup starts immediately after each successful push.
- Backup is asynchronous and must not make Git availability depend on OneDrive.
- Local-first and server modes belong to the same product.
- Local-first mode is single-machine and compliance-oriented.
- Server mode supports multiple personal devices.
- The product provides Git hosting only, not GitHub-style collaboration.
- OneDrive is the first provider.
- Storage providers must be replaceable through an adapter boundary.
- Backup artifacts must be verified and immutable.
- Clean-machine recovery is a core feature, not a later operational script.
- Backup must cover committed Git data; uncommitted working-tree data is outside scope.
- The product must use standard Git-compatible data formats where possible.

## 24. Open implementation decisions

The implementation team must resolve and document:

- The primary Git transport for each deployment mode.
- The supported operating systems for the first release.
- Whether OneDrive integration uses a synchronized folder, Microsoft Graph, or another mature transport.
- The local operational-state database and durable queue mechanism.
- The exact strategy for obtaining a consistent repository snapshot during concurrent pushes.
- The manifest serialization format and versioning strategy.
- The mature encryption library or format used for optional encryption.
- How credentials integrate with each supported operating system.
- Packaging, service installation, automatic startup, and upgrades.
- Default retention behavior beyond the safe requirement not to delete the only valid backup.
- Repository size limits or warnings for full-bundle backups.

These choices must preserve the product invariants and acceptance criteria in this document.

## 25. Future considerations

Potential later capabilities include:

- S3-compatible and NAS storage adapters.
- Incremental bundle chains with independently verified recovery plans.
- Content deduplication.
- Scheduled automated restore drills.
- Storage-cost estimation and retention simulation.
- Backup replication to more than one provider.
- Read-only mirror exports.
- Administrative web UI.
- Notifications through operating-system, email, or webhook channels.
- Multi-user authentication without adding broader collaboration features.

None of these should delay the core MVP: standard Git push and pull, immediate verified backup, transparent status, and clean-machine recovery.
