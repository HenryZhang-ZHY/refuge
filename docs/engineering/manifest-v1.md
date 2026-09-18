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
3. Copy it as `*.bundle.partial`, flush it, and rename it to `*.bundle`.
4. Write and flush `*.manifest.json.partial`, then rename it to `*.manifest.json`.

An artifact without a manifest is an unpublished orphan and may be cleaned up. A manifest whose
artifact is absent or whose size or checksum differs is corrupt and must not be restored.

## Layout

```text
<target>/refuge/v1/repos/<repo-id>/
  repo.json
  snapshots/<snapshot-id>.bundle
  snapshots/<snapshot-id>.manifest.json
```

`repo-id` is a UUID v7 stored in the bare repository as `refuge.repoid`. Snapshot IDs have the
form `<YYYYMMDDTHHMMSSZ>-g<generation>-<instance-short>`. Generation starts at one and is one
greater than the highest valid manifest already present for that repository.

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
  "encryption": null,
  "refuge_version": "0.1.0"
}
```

For an empty repository, `artifact` is `null`; `refs` may still contain the symbolic `HEAD`.
The ref-state hash covers the sorted `(refname, object-id)` pairs and symbolic HEAD target using
the `refuge-ref-state-v1` domain separator.

Encryption is not supported in iteration 0. A later encrypted manifest keeps this public
envelope, replaces `encryption: null` with its scheme metadata, and may move `refs` into an
encrypted sidecar referenced by a new field.
