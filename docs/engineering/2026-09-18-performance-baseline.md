---
title: Refuge Performance Baseline
status: Initial smoke baseline
date: 2026-09-18
---

# Refuge Performance Baseline

This baseline exists to prevent speculative cache or database work. It is not
a performance target. Re-run `scripts/benchmark.sh` with representative
repository counts, history, bundle sizes, and LFS content before choosing an
optimization.

Initial environment: Linux 6.18 x86_64, Rust 1.94.1, Git 2.39.5, release
build. The smoke fixture used three repositories and five snapshots per
repository; its objects are intentionally tiny.

| Operation | Wall time | Git subprocesses |
| --- | ---: | ---: |
| `repo status --all` | 12.2 ms | 9 |
| `snapshots list` | 2.0 ms | 0 |
| `repo backup repo-1` | 42.7 ms | 33 |

This run shows only that the harness works; the fixture is too small for an
indexing decision. Git subprocess counts are Git Trace2 `start` events. Scale
studies should vary repository count, manifests per
repository, reachable Git objects, bundle bytes, LFS object count/bytes, and
target filesystem. Record cold and warm filesystem-cache runs separately.

The snapshot directory and manifests remain the source of truth. Any future
index must be rebuildable and may not be the sole evidence that data is
protected.
