---
id: 98
title: A command cannot see which files changed
type: feature
status: backlog
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

- [ ] A restarted node sees the paths that caused it, and only those
- [ ] The variable never changes a cache key
