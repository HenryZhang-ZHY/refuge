---
title: Refuge Manifest v1
status: Frozen
date: 2026-09-18
---

# Refuge Manifest v1

Manifest v1 is the compatibility boundary for Refuge snapshots. Writers may add fields, but may
not remove fields or change their meaning. Readers must ignore unknown fields.

## Publication contract

A snapshot exists only when its `*.manifest.json` file exists. Writers publish in this order:

1. Create and verify the Git bundle in staging.
2. Calculate its SHA-256 checksum and byte size.
3. Copy it to a unique partial file on the destination volume, flush and
   re-read it, then commit it to the final `*.bundle` path without overwrite.
4. If present, publish the verified Git LFS archive by the same process.
5. Write and flush a unique partial manifest, then commit it without replacing
   any existing path. The manifest is always published last.

An artifact without a manifest is an unpublished orphan and may be cleaned up. A manifest whose
artifact is absent or whose size or checksum differs is corrupt and must not be restored.
Refuge re-reads destination bytes before commit. File flush failure prevents
commit; a directory-flush or cleanup failure after commit is reported as a
committed snapshot with warnings. This establishes local filesystem
publication, not confirmation that a sync client uploaded the files.

## Layout

```text
<target>/refuge/v1/repos/<repo-id>/
  repo.json
  snapshots/<snapshot-id>.bundle
  snapshots/<snapshot-id>.lfs.tar
  snapshots/<snapshot-id>.manifest.json
```

`repo-id` is a UUID v7 stored in the bare repository as `refuge.repoid`. Snapshot IDs have the
form `<YYYYMMDDTHHMMSSZ>-g<generation>-<instance-short>`. Generation starts at one and is one
greater than the highest valid manifest already present for that repository.
Generation is scoped to one repository and coordinated writer namespace. It is
not a global timestamp or a total order across Refuge instances; readers use
`(generation, snapshot_id)` only within one repository catalog.

## Envelope

```json
{
  "schema_version": 1,
  "repo_id": "0192d23c-0000-7000-8000-000000000000",
  "repo_name": "beancount",
  "instance_id": "0192d23d-0000-7000-8000-000000000000",
  "snapshot_id": "20260918T091530Z-g42-a1b2c3d4",
  "created_at": "2026-09-18T09:15:30Z",
  "generation": 42,
  "ref_state_hash": "sha256:...",
  "refs": {
    "HEAD": { "symref": "refs/heads/main" },
    "refs/heads/main": "0123456789abcdef..."
  },
  "artifact": {
    "key": "snapshots/20260918T091530Z-g42-a1b2c3d4.bundle",
    "size": 123456,
    "checksum": "sha256:...",
    "format": "git-bundle",
    "format_version": 2
  },
  "lfs_artifact": {
    "key": "snapshots/20260918T091530Z-g42-a1b2c3d4.lfs.tar",
    "size": 654321,
    "checksum": "sha256:...",
    "format": "lfs-archive",
    "format_version": 1
  },
  "encryption": null,
  "refuge_version": "0.1.0"
}
```

For an empty repository, `artifact` is `null`; `refs` may still contain the symbolic `HEAD`.
The ref-state hash covers the sorted `(refname, object-id)` pairs and symbolic HEAD target using
the `refuge-ref-state-v1` domain separator.

`lfs_artifact` is optional so pre-LFS v1 manifests remain readable. New
backups derive required LFS object ids and sizes from pointers in all history
reachable through the snapshot refs. Every required object must exist and
match both size and SHA-256 before publication. Archives contain only explicit
regular-file entries under the canonical `<oid[0:2]>/<oid[2:4]>/<oid>` layout.

## Validation and restore selection

Quick catalog inspection validates manifest structure and semantics plus
artifact presence and size. It does not claim content verification. Complete
restore verification copies a candidate into isolated staging, checks checksum,
performs `git bundle verify`, mirror clone, HEAD and refs/hash equality, `git
fsck --full --strict`, controlled LFS extraction, and required-object checks.

An explicitly selected snapshot fails on any validation error. Default restore
tries candidates newest-first and may fall back after content corruption,
reporting every skipped newer snapshot and the actual restored id. Environmental
failures such as permission denial stop restoration instead of silently
selecting older history. Unsupported schema versions or encryption are
reported separately from corrupt content.

Encryption is not supported in iteration 0. A later encrypted manifest keeps this public
envelope, replaces `encryption: null` with its scheme metadata, and may move `refs` into an
encrypted sidecar referenced by a new field.
