---
title: Correctness Refactoring Release Notes
date: 2026-09-18
---

# Correctness Refactoring Release Notes

This release preserves manifest schema v1 and existing CLI arguments, but
tightens what Refuge is willing to call protected or restore.

- Backup staging is isolated per operation. Published paths are never
  overwritten, destination bytes are re-read before commit, and the manifest
  remains the commit marker. Post-commit cleanup or directory-durability
  problems are warnings rather than false “backup failed” results.
- Manifests now receive semantic validation. Unknown additive fields remain
  compatible; unsupported schema versions and encryption are explicit errors.
  A damaged manifest no longer blocks unrelated repository discovery.
- Default restore verifies candidates completely and falls back from content
  corruption to an older snapshot with a warning. `--snapshot` never falls
  back. Permission and other environmental errors stop the operation.
- Git LFS protection now requires every object referenced anywhere in reachable
  history. A repository with a missing pointer target is reported as
  incompletely protected even if older Refuge versions published a v1
  manifest without an LFS archive.
- Restoring under another name preserves identity and is rejected while the
  original identity is active. Creating a fork with a new identity is not part
  of `--as`.

Older Refuge readers can still parse new v1 manifests if they ignore unknown
fields, but they may not enforce LFS completeness, no-overwrite publication,
or candidate verification at the same level. Do not use an older writer
concurrently in the same repository snapshot namespace: generation numbers are
writer-coordinated within that namespace, not a global clock.
