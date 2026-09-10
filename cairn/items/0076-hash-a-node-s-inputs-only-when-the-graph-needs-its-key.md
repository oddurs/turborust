---
id: 76
title: Hash a node's inputs only when the graph needs its key
type: chore
status: backlog
milestone: later
created: 2026-09-10
updated: 2026-09-10
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

- [ ] A node's inputs are not read unless its key is required
- [ ] A filtered-out node costs no file reads
- [ ] Measured on a workspace of twenty or more crates, before and after
