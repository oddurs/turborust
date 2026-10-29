---
id: 98
title: A command cannot see which files changed
type: feature
status: done
created: 2026-10-28
updated: 2026-10-28
priority: p3
effort: s
area: watch
---

## Problem

When a change restarts a node, the command is told nothing about the change.
A test task that could run only the tests near the edited file, or a formatter
that could check only what changed, has to rediscover it.

## Precedent

watchexec `--emit-events-to=environment`, which sets `WATCHEXEC_WRITTEN_PATH`
and friends, and `--emit-events-to=json-stdio`.

## Proposal

`TURBORUST_CHANGED` — newline-separated, workspace-relative paths that matched
the node's globs — on a restart caused by a change, and empty otherwise. Not
part of any cache key: it describes why the node ran, not what it ran on.

## Acceptance criteria

- [x] A restarted node sees the paths that caused it, and only those
- [x] The variable never changes a cache key

## 2026-10-29

Dispatch records, for every node a change matched, the matching paths; the node
takes them when its next process starts, as `TURBORUST_CHANGED` (sorted,
newline-separated, workspace-relative). Paths accumulate until taken, so a
debounce that folds two batches into one restart reports both. A start no change
caused sees an empty value; the variable is always set, so a command can test
for it without `${VAR:-}`.

Two choices that were not obvious. Nodes restarted by the cascade get their own
matches, not their upstream's: a dependent whose globs did not match is told
nothing, which is what "only those" asks. And a task takes its paths at the
start of `run_task`, hit or miss: left behind on a cache hit, they would be
handed to some later run that no change caused.

It is inserted by `spawn_env`, beside `child_env` rather than inside it, so
readiness and liveness probes and the cache key never see it. A task that lists
`TURBORUST_CHANGED` in `env_keys` reads the ambient variable, not this one;
the key test covers exactly that case.

Mutation-checked: not recording the paths, and not forgetting them once taken,
each fail a test.
