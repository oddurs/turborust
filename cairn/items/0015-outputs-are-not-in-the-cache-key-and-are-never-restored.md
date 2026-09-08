---
id: 15
title: Outputs are not in the cache key and are never restored
type: bug
status: done
milestone: v0.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p1
effort: l
area: cache
---

## Problem

Two related holes, both reproduced:

**1. `outputs` is not part of the key.** Narrowing the declared outputs leaves the
key unchanged, so the run is a hit even though the previous run produced files the
new declaration no longer covers.

**2. The cache never stores outputs — it only checks they still exist.** So a hit
cannot restore anything:

```
# outputs = ["out.txt", "second.txt"] -> both produced
# narrow to outputs = ["out.txt"], delete second.txt
$ turborust run build
   build | [turborust] cache hit  b3:e3d5311cc79d  (saved 29ms)
$ ls second.txt
NO
```

The tree is now in a state the task would never have produced, and we called it a
hit. It also means `cargo clean` turns every hit into a permanent miss, because
there is nothing to replay.

## Proposal

- Fold the declared `outputs` globs into the key.
- Archive the outputs on a successful run and restore them on a hit, so a hit is a
  real replay rather than a bet that the tree was not touched.

This is the prerequisite for 0013 (shared cache): there is nothing to share until
there is an artifact to share.

## Acceptance criteria

- [x] `outputs` participates in the cache key
- [x] A successful run archives its declared outputs
- [x] A hit restores them, and restores nothing else
- [x] Deleting an output and re-running reproduces it from cache
- [x] A test covers the narrowing case above

## 2026-09-08

Both halves fixed.

Key: declared outputs and env_mode now participate, so narrowing outputs is a miss rather than a stale replay.

Storage: records moved from one-file-per-task to hash-addressed .turborust/runs/<task>/<key>.json with artifacts alongside in .turborust/artifacts/<task>/<key>/. That fixes the problem found while closing 0014 — the old layout kept only the last result, so alternating between two input sets missed every time. Ten results retained per task, pruned oldest-first with their artifacts, so a branch switch is fast without unbounded disk.

A hit now replays: it copies the archived files back rather than checking they survived. Verified by deleting both outputs and re-running — 'cache hit b3:131d8ac1f8c0 (saved 46ms, 2 output(s) restored)'.

Two deliberate calls:

1. Output collection does NOT respect .gitignore, unlike input hashing. Build outputs are almost always ignored, so honouring it would archive nothing and silently make every task un-restorable. This asymmetry is intentional and commented at the call site.

2. Results over 256 MiB are recorded but not archived, and such a record falls back to the old outputs-present check. A dev cache that can quietly eat the disk is worse than one that occasionally rebuilds; the log says which happened.

This unblocks 0013 — there is now an artifact to share.
