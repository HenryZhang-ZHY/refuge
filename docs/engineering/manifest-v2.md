# Refuge snapshot manifest v2

Refuge stores one append-only snapshot catalog per repository at
`<target>/refuge/v2/repos/<repo-id>/`. A recovery point exists only after its
`snapshots/<snapshot-id>.json` manifest has been durably published. All bundle,
LFS object, and LFS set dependencies are immutable and are written first.

The manifest contains the repository and instance UUIDs, generation and
snapshot ID, the exact ref state and its domain-separated SHA-256 hash, a Git
parent plus optional bundle descriptor, and an optional LFS set descriptor.
Artifact descriptors contain only `key`, `size`, and `checksum`.

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
    "refs/heads/main": "0123456789abcdef0123456789abcdef01234567"
  },
  "git": {
    "parent": "20260919T090000Z-g41-a1b2c3d4",
    "bundle": { "key": "git/20260919T103000Z-g42-a1b2c3d4.bundle", "size": 204800, "checksum": "sha256:…" }
  },
  "lfs": {
    "set": { "key": "lfs/sets/<digest>.txt", "size": 451200, "checksum": "sha256:<digest>" },
    "count": 5012,
    "size": 629145600
  }
}
```

A null Git parent denotes a checkpoint. A non-null parent denotes a delta; a
null bundle on a delta denotes a refs-only change. Empty repositories have no
bundle and no LFS section. LFS set files are canonical, sorted lines of
`<lowercase-sha256-oid> <decimal-size>\n` and are named by the SHA-256 of their
complete contents.

Readers ignore unknown JSON fields but reject any schema version other than 2.
Refuge does not read the pre-release v1 layout and provides no migration path.
