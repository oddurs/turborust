---
id: 76
title: Hash a node's inputs only when the graph needs its key
type: chore
status: done
milestone: later
created: 2026-09-10
updated: 2026-10-29
priority: p3
effort: m
area: cache
---

## Problem

Input hashing happens up front. On a large workspace that is a burst of file
reads before anything useful starts, including for nodes the run will never
reach — filtered out, downstream of a failure, or simply not selected.

Turborepo shipped deferred input hashing in 2.10 for this.

## What should happen

Hash a node's inputs at the moment its key is first needed, and cache the result
for the rest of the session.

Worth noting where this project already is: moon's companion change was to stop
delegating hashing to git and do it natively, which they measured at 10-50%.
turborust already hashes content itself — deliberately, because mtime-based
invalidation is the bug this cache exists to avoid — so only the deferral part
applies here.

## Why this is p3

It is a latency improvement on a cost that is already paid in parallel with
process startup. It is worth doing and it is not worth doing before anything
else in v1.3 or v1.4. Filed so it is not rediscovered as a new idea.

## Acceptance criteria

- [x] A node's inputs are not read unless its key is required
- [x] A filtered-out node costs no file reads
- [x] Measured on a workspace of twenty or more crates, before and after

## 2026-10-29

The deferral this item asks for was already true when I looked, and the real cost was somewhere the item did not name.

**Already deferred.** `Engine::fingerprint` has three callers: `run_task`, as the task is about to run; `why`, for its one target; and `--dry-run`, which is asked for every key. A node outside the selection is not in the resolved plan at all, and a node downstream of a failure never reaches `run_task`. So no node's inputs are read before its key is needed, and a filtered-out node costs nothing. Those two criteria were true by construction; nothing new was needed for them.

**The cost that was there: every hash walked the whole workspace.** `fingerprint_files` started an `ignore` walk at the root and filtered each file through the node's globs afterwards. `crates/api/src/**` read the directory entries of every non-gitignored file in the repository (docs, assets, fixtures, other crates), once per task hashed. Now each include glob yields its literal directory prefix (`crates/api/src/**/*.rs` → `crates/api/src`; `**/*.rs` → anywhere), and the walk prunes any directory that is neither on the way to one nor inside one. That is sound even though globset's `*` crosses `/`, since whatever a glob matches still starts with its literal prefix. The walk still starts at the root with `parents(false)`, so which `.gitignore`s apply is exactly what it was. Starting the walk at each prefix instead would have silently stopped applying the root `.gitignore` to subtrees, or with `parents(true)` started applying ignore files above the workspace, and either would change keys.

`a_pruned_walk_hashes_exactly_what_a_full_one_did` keeps the old walk as a reference and compares them over nine glob shapes, including a root `.gitignore` reaching into a pruned subtree and a sibling with a shared name prefix (`crates/api` and `crates/apix`). Dropping the "on the way down" clause makes it fail.

**Measured** on 20 crates of 30 files each, plus 20,000 unrelated, non-ignored files, release builds, medians of 15 interleaved runs:

| | before | after |
|---|---|---|
| `run --dry-run=json` (20 keys) | 517 ms | 79 ms |
| `run c00`, cached | 101 ms | 69 ms |

The 20 keys are byte-identical before and after.

